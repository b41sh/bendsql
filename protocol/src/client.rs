// Copyright 2021 Datafuse Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::BTreeMap;

use crate::capability::Capability;
use crate::config::ClientConfig;
use crate::error::{Error, Result};
use crate::http::{self, is_header_value, HttpRequest, HttpResponse, Method};
use crate::query::{Query, QueryOptions};
use crate::request::{LoginRequest, QueryRequest, RefreshSessionTokenRequest};
use crate::response::{LoginResponseResult, RefreshResponse, SessionTokenInfo};
use crate::session::SessionState;

/// The most cookies kept; more are ignored, so a server can't grow every request without end.
const MAX_COOKIES: usize = 32;
/// The longest cookie (name and value) kept.
const MAX_COOKIE_BYTES: usize = 4096;

/// What a login returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginInfo {
    /// The server's version (`1.2.835-nightly-f5457e85f4`), when it said.
    pub version: Option<String>,
    /// What that version supports; nothing optional when the version is unknown.
    pub capability: Capability,
}

/// The state of one connection: its session, session tokens, cookies, last node and route
/// hint. It builds the requests and reads what the server says back about the session; one
/// statement's paging is a [`Query`]. It does no locking: a caller sharing it between threads
/// puts it behind a lock.
pub struct Client {
    endpoint: String,
    config: ClientConfig,
    session: SessionState,
    warehouse: Option<String>,
    tokens: Option<SessionTokenInfo>,
    cookies: BTreeMap<String, String>,
    last_node_id: Option<String>,
    route_hint: String,
    session_id: Option<String>,
    rng: u64,
    hint_nonce: u64,
}

impl Client {
    /// A connection with these settings. `seed` makes the query ids and route hints: pass some
    /// randomness (the time in nanoseconds, a random number), different for every client, since
    /// two clients with one seed send the same query ids. Tests pass a constant.
    pub fn new(config: ClientConfig, seed: u64) -> Result<Client> {
        let endpoint = config.validate()?;
        let mut cookies = BTreeMap::new();
        // as bendsql has always sent it
        cookies.insert("cookie_enabled".to_string(), "true".to_string());
        let mut client = Client {
            endpoint,
            session: config.session.clone(),
            warehouse: config.warehouse.clone(),
            config,
            tokens: None,
            cookies,
            last_node_id: None,
            route_hint: String::new(),
            session_id: None,
            rng: seed,
            hint_nonce: 0,
        };
        client.route_hint = client.next_route_hint();
        Ok(client)
    }

    /// The session as the server last returned it: database, role, settings, transaction.
    pub fn session(&self) -> &SessionState {
        &self.session
    }

    /// The session id a login returned, for logs.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// How long the session token lasts after a login or refresh, when there is one: a caller
    /// with a clock may refresh before then rather than after an `Error::TokenExpired`.
    pub fn session_token_ttl_secs(&self) -> Option<u64> {
        self.tokens.as_ref().map(|t| t.session_token_ttl_in_secs)
    }

    /// The largest response body read: a transport can stop reading there.
    pub fn max_body_bytes(&self) -> usize {
        self.config.max_body_bytes
    }

    /// `http(s)://host:port`, as configured.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The login request (`POST /v1/session/login`), for a caller that wants session tokens or
    /// the server's version. Without a login every request carries the configured `Auth`.
    pub fn login_request(&self) -> Result<HttpRequest> {
        let mut path = "/v1/session/login".to_string();
        if !self.config.session_tokens {
            path.push_str("?disable_session_token=true");
        }
        let body = serde_json::to_vec(&LoginRequest::from(&self.session))
            .map_err(|e| Error::Config(format!("can't encode the login: {e}")))?;
        let mut headers = self.base_headers(None);
        headers.push((http::AUTHORIZATION, self.config.auth.header_value()));
        headers.push((http::CONTENT_TYPE, http::CONTENT_TYPE_JSON.to_string()));
        Ok(self.request(Method::Post, &path, headers, Some(body), true))
    }

    /// Reads the login's response: keeps the session tokens and cookies, and returns the
    /// server's version. A 404 (a server older than login) is `Ok` with no version.
    pub fn on_login(&mut self, response: HttpResponse) -> Result<LoginInfo> {
        self.observe(&response)?;
        if response.status == 404 {
            return Ok(LoginInfo {
                version: None,
                capability: Capability::default(),
            });
        }
        if response.status != 200 {
            return Err(Error::from_status(response.status, &response.body, false));
        }
        let info = match decode::<LoginResponseResult>(&response.body, "login")? {
            LoginResponseResult::Ok(info) => info,
            LoginResponseResult::Err { error } => return Err(Error::Login(error)),
        };
        if self.config.session_tokens {
            if let Some(tokens) = info.tokens {
                check_token(&tokens)?;
                self.tokens = Some(tokens);
            }
        }
        self.session_id = response
            .header(http::HEADER_SESSION_ID)
            .filter(|id| is_header_value(id))
            .map(str::to_string);
        Ok(LoginInfo {
            capability: Capability::from_server_version(&info.version),
            version: Some(info.version),
        })
    }

    /// After an `Error::TokenExpired`: the refresh request (`POST /v1/session/refresh`), then
    /// `on_refresh`, then the failed request again (`Query::retry_request`). `None` without
    /// session tokens.
    pub fn refresh_request(&self) -> Result<Option<HttpRequest>> {
        let Some(tokens) = &self.tokens else {
            return Ok(None);
        };
        let body = serde_json::to_vec(&RefreshSessionTokenRequest {
            session_token: tokens.session_token.clone(),
        })
        .map_err(|e| Error::Config(format!("can't encode the refresh: {e}")))?;
        let mut headers = self.base_headers(None);
        let bearer = format!("Bearer {}", tokens.refresh_token.inner());
        headers.push((http::AUTHORIZATION, bearer));
        headers.push((http::CONTENT_TYPE, http::CONTENT_TYPE_JSON.to_string()));
        let path = "/v1/session/refresh";
        Ok(Some(self.request(
            Method::Post,
            path,
            headers,
            Some(body),
            true,
        )))
    }

    /// Reads the refresh's response and keeps the new tokens. A refusal is `Error::Login`: the
    /// refresh token expired too, and only a new login helps.
    pub fn on_refresh(&mut self, response: HttpResponse) -> Result<()> {
        self.observe(&response)?;
        if response.status != 200 {
            return Err(Error::from_status(response.status, &response.body, false));
        }
        match decode::<RefreshResponse>(&response.body, "session refresh")? {
            RefreshResponse::Ok(tokens) => {
                check_token(&tokens)?;
                self.tokens = Some(tokens);
                Ok(())
            }
            RefreshResponse::Err { error } => Err(Error::Login(error)),
        }
    }

    /// The logout request (`POST /v1/session/logout`), when the session holds server state to
    /// release: session tokens, or temporary tables (`need_keep_alive`). Send it once, when done.
    pub fn logout_request(&self) -> Option<HttpRequest> {
        if self.tokens.is_none() && !self.session.need_keep_alive() {
            return None;
        }
        let mut headers = self.base_headers(None);
        headers.push((http::AUTHORIZATION, self.auth_header()));
        if self.session.need_sticky() {
            if let Some(node) = &self.last_node_id {
                headers.push((http::HEADER_STICKY_NODE, node.clone()));
            }
        }
        Some(self.request(Method::Post, "/v1/session/logout", headers, None, true))
    }

    /// Starts a statement: its paging state and the first request (`POST /v1/query`).
    pub fn start_query(
        &mut self,
        sql: &str,
        options: QueryOptions,
    ) -> Result<(Query, HttpRequest)> {
        // a transaction's statements keep their route; others may go anywhere
        if !self.session.in_active_transaction() {
            self.route_hint = self.next_route_hint();
        }
        let query_id = self.next_query_id();
        let request = QueryRequest::new(sql)
            .with_pagination(self.config.pagination.clone())
            .with_session(Some(self.session.clone()))
            .with_stage_attachment(options.stage_attachment)
            .with_params(options.params);
        let body = serde_json::to_vec(&request)
            .map_err(|e| Error::Config(format!("can't encode the query: {e}")))?;
        let sticky = if self.session.need_sticky() {
            self.last_node_id.clone()
        } else {
            None
        };
        let query = Query::new(query_id, body, sticky);
        let request = query
            .retry_request(self)
            .ok_or_else(|| Error::Config("the query has no request to send".into()))?;
        Ok((query, request))
    }

    /// The headers every request carries, with the query id when there is one.
    pub(crate) fn base_headers(&self, query_id: Option<&str>) -> Vec<(&'static str, String)> {
        let mut headers = Vec::with_capacity(8);
        if let Some(tenant) = &self.config.tenant {
            headers.push((http::HEADER_TENANT, tenant.clone()));
        }
        if let Some(warehouse) = &self.warehouse {
            headers.push((http::HEADER_WAREHOUSE, warehouse.clone()));
        }
        headers.push((http::HEADER_ROUTE_HINT, self.route_hint.clone()));
        if let Some(id) = query_id {
            headers.push((http::HEADER_QUERY_ID, id.to_string()));
        }
        if !self.cookies.is_empty() {
            let cookies: Vec<String> = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            headers.push((http::COOKIE, cookies.join("; ")));
        }
        headers
    }

    /// The `authorization` value for a query: the session token after a login gave one, else the
    /// configured `Auth`.
    pub(crate) fn auth_header(&self) -> String {
        match (&self.tokens, self.config.session_tokens) {
            (Some(tokens), true) => format!("Bearer {}", tokens.session_token.inner()),
            _ => self.config.auth.header_value(),
        }
    }

    /// Whether queries authenticate with a session token, which a 401 may ask to refresh.
    pub(crate) fn uses_session_token(&self) -> bool {
        self.tokens.is_some() && self.config.session_tokens
    }

    /// A request to `path` (already checked) on the endpoint.
    pub(crate) fn request(
        &self,
        method: Method,
        path: &str,
        headers: Vec<(&'static str, String)>,
        body: Option<Vec<u8>>,
        replayable: bool,
    ) -> HttpRequest {
        HttpRequest {
            method,
            url: format!("{}{path}", self.endpoint),
            headers,
            body,
            replayable,
        }
    }

    /// Checks a response's size and keeps the cookies it sets.
    pub(crate) fn observe(&mut self, response: &HttpResponse) -> Result<()> {
        if response.body.len() > self.config.max_body_bytes {
            return Err(Error::Protocol(format!(
                "a response of {} bytes, above the limit of {}",
                response.body.len(),
                self.config.max_body_bytes
            )));
        }
        for line in response.headers_named(http::SET_COOKIE) {
            let pair = line.split(';').next().unwrap_or("");
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            let usable = !name.is_empty()
                && name.len() + value.len() <= MAX_COOKIE_BYTES
                && is_header_value(name)
                && is_header_value(value)
                && !name.contains([' ', '\t', ',', '='])
                && (self.cookies.len() < MAX_COOKIES || self.cookies.contains_key(name));
            if usable {
                self.cookies.insert(name.to_string(), value.to_string());
            }
        }
        Ok(())
    }

    /// Takes the session a query response returned (the final page returns none), and the
    /// warehouse it may have switched to.
    pub(crate) fn update_session(&mut self, session: Option<SessionState>) -> Result<()> {
        let Some(session) = session else {
            return Ok(());
        };
        if let Some(warehouse) = session.settings.as_ref().and_then(|s| s.get("warehouse")) {
            if !is_header_value(warehouse) {
                return Err(Error::Protocol(
                    "the session names a warehouse a request header can't carry".into(),
                ));
            }
            self.warehouse = Some(warehouse.clone());
        }
        self.session = session;
        Ok(())
    }

    /// The node that ran the last query, for sticky sessions.
    pub(crate) fn set_last_node_id(&mut self, node_id: &str) {
        self.last_node_id = Some(node_id.to_string());
    }

    /// The route hint a query's first response returned.
    pub(crate) fn set_route_hint(&mut self, hint: &str) {
        self.route_hint = hint.to_string();
    }

    /// splitmix64 (Steele, Lea and Flood, "Fast splittable pseudorandom number generators",
    /// OOPSLA 2014): ids that differ, not secrets.
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 32 hex digits, the shape of the server's own ids.
    fn next_query_id(&mut self) -> String {
        format!("{:016x}{:016x}", self.next_u64(), self.next_u64())
    }

    fn next_route_hint(&mut self) -> String {
        let nonce = self.hint_nonce;
        self.hint_nonce = self.hint_nonce.wrapping_add(1);
        format!(
            "rh:{:016x}{:016x}:{nonce:06}",
            self.next_u64(),
            self.next_u64()
        )
    }
}

impl std::fmt::Debug for Client {
    // cookies and tokens stay out
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("endpoint", &self.endpoint)
            .field("session", &self.session)
            .field("warehouse", &self.warehouse)
            .field("session_tokens", &self.tokens.is_some())
            .field("cookies", &self.cookies.len())
            .field("last_node_id", &self.last_node_id)
            .finish()
    }
}

/// A JSON body of the protocol; `what` names it in the error. The error says where the JSON
/// went wrong but quotes none of it: a login's body holds session tokens.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| {
        Error::Protocol(format!(
            "the {what} response isn't the protocol's JSON ({:?} error at line {}, column {})",
            e.classify(),
            e.line(),
            e.column()
        ))
    })
}

/// Tokens go in a header: they must be able to.
fn check_token(tokens: &SessionTokenInfo) -> Result<()> {
    let usable = |t: &str| !t.is_empty() && is_header_value(t);
    if usable(tokens.session_token.inner()) && usable(tokens.refresh_token.inner()) {
        Ok(())
    } else {
        Err(Error::Protocol(
            "the server sent a session token a request header can't carry".into(),
        ))
    }
}
