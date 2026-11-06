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

use crate::client::{decode, Client};
use crate::error::{quote_body, Error, Result};
use crate::http::{self, is_header_value, HttpRequest, HttpResponse, Method};
use crate::request::StageAttachmentConfig;
use crate::response::{QueryResponse, QueryStats, SchemaField};
use crate::settings::QueryResultFormatSettings;

/// The longest next, final or kill path accepted.
const MAX_PATH_LEN: usize = 1024;
/// The longest query or node id accepted.
const MAX_ID_LEN: usize = 128;

/// What a statement sends besides its SQL.
#[derive(Debug, Default)]
pub struct QueryOptions<'a> {
    /// Values for `?` or `:name` placeholders, bound by the server (servers with
    /// `Capability::server_side_params`).
    pub params: Option<serde_json::Value>,
    /// A stage attachment, for `INSERT … FROM @stage` loads.
    pub stage_attachment: Option<StageAttachmentConfig<'a>>,
}

/// What one response gave.
#[derive(Debug)]
pub struct Step {
    /// The rows of this page, a cell per column of [`Query::schema`]; `None` is SQL `NULL`.
    pub rows: Vec<Vec<Option<String>>>,
    pub stats: QueryStats,
    /// The server's warnings, given with the first response.
    pub warnings: Vec<String>,
    /// What to send next.
    pub next: Next,
}

/// The request that follows a response.
#[derive(Debug)]
pub enum Next {
    /// The next page (or the query's state while it runs without rows yet).
    Page(HttpRequest),
    /// The final request, which releases the result on the server. Every row has come; a
    /// caller that only wants the rows may send it without waiting, though its response is
    /// what ends the query here.
    Final(HttpRequest),
    /// The query is over.
    Done,
}

/// The request a query waits on.
#[derive(Debug, Clone)]
enum Pending {
    Start {
        body: Vec<u8>,
        sticky: Option<String>,
    },
    Get {
        path: String,
        is_final: bool,
    },
    Done,
}

/// One statement, a response at a time. It holds no rows: each [`Step`] hands them over.
#[derive(Debug)]
pub struct Query {
    /// The id sent in `x-databend-query-id`; the server's own once it answers.
    id: String,
    answered: bool,
    node_id: Option<String>,
    kill_path: Option<String>,
    pending: Pending,
    finished: bool,
    schema: Vec<SchemaField>,
    settings: QueryResultFormatSettings,
}

impl Query {
    pub(crate) fn new(id: String, body: Vec<u8>, sticky: Option<String>) -> Self {
        Self {
            id,
            answered: false,
            node_id: None,
            kill_path: None,
            pending: Pending::Start { body, sticky },
            finished: false,
            schema: Vec::new(),
            settings: QueryResultFormatSettings::default(),
        }
    }

    /// The query's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The node running it, once the server said.
    pub fn node_id(&self) -> Option<&str> {
        self.node_id.as_deref()
    }

    /// The columns, once a response has said them.
    pub fn schema(&self) -> &[SchemaField] {
        &self.schema
    }

    /// How the server formatted the cells (binary and geometry encodings, time zone).
    pub fn settings(&self) -> &QueryResultFormatSettings {
        &self.settings
    }

    /// Whether the query is over: its final response came, or it needs none.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// The request the query waits on, built afresh with the client's current credentials and
    /// cookies: to send it again after an error `is_retryable()` (a 503, a refreshed session
    /// token). `None` once the query is over or failed for good.
    pub fn retry_request(&self, client: &Client) -> Option<HttpRequest> {
        let mut headers = client.base_headers(Some(&self.id));
        headers.push((http::AUTHORIZATION, client.auth_header()));
        match &self.pending {
            Pending::Start { body, sticky } => {
                headers.push((http::CONTENT_TYPE, http::CONTENT_TYPE_JSON.to_string()));
                if let Some(node) = sticky {
                    headers.push((http::HEADER_STICKY_NODE, node.clone()));
                }
                let body = Some(body.clone());
                // after a connection error the statement may have run: not replayable
                Some(client.request(Method::Post, "/v1/query", headers, body, false))
            }
            Pending::Get { path, .. } => {
                if let Some(node) = &self.node_id {
                    headers.push((http::HEADER_STICKY_NODE, node.clone()));
                }
                Some(client.request(Method::Get, path, headers, None, true))
            }
            Pending::Done => None,
        }
    }

    /// Stops a query that hasn't finished (a timeout, the caller gave up, an error):
    /// `POST /v1/query/{id}/kill`. `None` once it is over.
    pub fn kill_request(&self, client: &Client) -> Option<HttpRequest> {
        if self.finished {
            return None;
        }
        let path = match &self.kill_path {
            Some(path) => path.clone(),
            None => format!("/v1/query/{}/kill", self.id),
        };
        let mut headers = client.base_headers(Some(&self.id));
        headers.push((http::AUTHORIZATION, client.auth_header()));
        if let Some(node) = &self.node_id {
            headers.push((http::HEADER_STICKY_NODE, node.clone()));
        }
        Some(client.request(Method::Post, &path, headers, None, true))
    }

    /// Reads the response to the last request this query gave, updating `client` (session,
    /// cookies, sticky node). On an error the query stops, except after one that
    /// `is_retryable()`, when `retry_request` sends the same request again; `kill_request`
    /// stops it on the server.
    pub fn on_response(&mut self, client: &mut Client, response: HttpResponse) -> Result<Step> {
        let pending = std::mem::replace(&mut self.pending, Pending::Done);
        let (was_start, was_final) = match &pending {
            Pending::Start { .. } => (true, false),
            Pending::Get { is_final, .. } => (false, *is_final),
            Pending::Done => {
                return Err(Error::Protocol(
                    "a response came for a query that sent no request".into(),
                ))
            }
        };
        client.observe(&response)?;
        if response.status != 200 {
            let error = if response.status == 404 && !was_start {
                Error::QueryNotFound(not_found_message(&response.body))
            } else {
                Error::from_status(response.status, &response.body, client.uses_session_token())
            };
            if error.is_retryable() {
                self.pending = pending;
            }
            return Err(error);
        }
        let resp: QueryResponse = decode(&response.body, "query")?;
        client.update_session(resp.session)?;
        let mut warnings = Vec::new();
        if !self.answered {
            self.on_first_response(client, &response, &resp.id, resp.node_id.as_deref())?;
            warnings = resp.warnings.unwrap_or_default();
        }
        if let Some(path) = &resp.kill_uri {
            self.kill_path = Some(check_path(path, "kill")?);
        }
        if let Some(error) = resp.error {
            return Err(Error::Query(error));
        }
        if !resp.schema.is_empty() {
            self.schema = resp.schema;
        }
        if let Some(settings) = resp.settings {
            self.settings = settings;
        }
        check_rows(&resp.data, self.schema.len())?;
        let next_path = match resp.next_uri {
            // the final response ends the query, whatever it says next
            Some(_) if was_final => None,
            Some(uri) => Some(check_path(&uri, "next")?),
            None => None,
        };
        let next = match next_path {
            None => {
                self.finished = true;
                Next::Done
            }
            Some(path) => {
                let is_final = path.ends_with("/final");
                self.pending = Pending::Get { path, is_final };
                let request = self.retry_request(client).ok_or_else(|| {
                    Error::Protocol("the query has no request to send next".into())
                })?;
                if is_final {
                    Next::Final(request)
                } else {
                    Next::Page(request)
                }
            }
        };
        Ok(Step {
            rows: resp.data,
            stats: resp.stats,
            warnings,
            next,
        })
    }

    /// Takes the server's id, node and route hint from the first response.
    fn on_first_response(
        &mut self,
        client: &mut Client,
        response: &HttpResponse,
        id: &str,
        node_id: Option<&str>,
    ) -> Result<()> {
        if !is_id(id) {
            return Err(Error::Protocol(format!(
                "the server named the query {:?}",
                quote_body(id.as_bytes())
            )));
        }
        id.clone_into(&mut self.id);
        if let Some(node) = node_id {
            if !is_header_value(node) || node.is_empty() || node.len() > MAX_ID_LEN {
                return Err(Error::Protocol(
                    "the server sent an unusable node id".into(),
                ));
            }
            self.node_id = Some(node.to_string());
            client.set_last_node_id(node);
        }
        if let Some(hint) = response.header(http::HEADER_ROUTE_HINT) {
            if is_header_value(hint) && hint.len() <= MAX_ID_LEN {
                client.set_route_hint(hint);
            }
        }
        self.answered = true;
        Ok(())
    }
}

/// The server's message for a 404, or the start of the body.
fn not_found_message(body: &[u8]) -> String {
    serde_json::from_slice::<crate::error::ResponseWithErrorCode>(body)
        .map(|r| r.error.message)
        .unwrap_or_else(|_| quote_body(body))
}

/// A query id that can go in a path segment: letters, digits, `-` and `_`.
fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A next, final or kill URI the server sent, accepted only as a path under `/v1/query/`, so
/// requests (and their credentials) never leave the configured endpoint.
fn check_path(uri: &str, what: &str) -> Result<String> {
    let bad = || {
        Error::Protocol(format!(
            "the {what} URI {:?} isn't a path under /v1/query/",
            quote_body(uri.as_bytes())
        ))
    };
    let Some(rest) = uri.strip_prefix("/v1/query/") else {
        return Err(bad());
    };
    let usable = uri.len() <= MAX_PATH_LEN
        && rest
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'));
    if usable {
        Ok(uri.to_string())
    } else {
        Err(bad())
    }
}

/// Every row has a cell per column, once the columns are known.
fn check_rows(rows: &[Vec<Option<String>>], columns: usize) -> Result<()> {
    if columns == 0 {
        return Ok(());
    }
    match rows.iter().position(|row| row.len() != columns) {
        None => Ok(()),
        Some(i) => Err(Error::Protocol(format!(
            "row {i} of a page has {} cells for {columns} columns",
            rows.get(i).map_or(0, Vec::len)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_must_stay_under_v1_query() {
        assert!(check_path("/v1/query/q1/page/1", "next").is_ok());
        assert!(check_path("/v1/query/q1", "next").is_ok());
        for bad in [
            "http://evil.example/v1/query/q1/page/1",
            "//evil.example/v1/query/q1",
            "/v1/query/../session/login",
            "/v1/query/q1/./final",
            "/v1/query//q1",
            "/v1/query/q1/page/1?x=1",
            "/v1/query/q1\\..\\x",
            "/v1/query/q1/%2e%2e",
            "/v1/session/logout",
            "/v1/query/",
            "",
        ] {
            assert!(check_path(bad, "next").is_err(), "{bad:?}");
        }
        let long = format!("/v1/query/{}", "a".repeat(MAX_PATH_LEN));
        assert!(check_path(&long, "next").is_err());
    }

    #[test]
    fn ids() {
        assert!(is_id("0193f0e3a1b2c3d4e5f60718293a4b5c"));
        assert!(is_id("q-1_2"));
        for bad in ["", "..", "a/b", "a b", "é"] {
            assert!(!is_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn rows_match_the_columns() {
        let row = |n: usize| vec![Some("x".to_string()); n];
        assert!(check_rows(&[row(2), row(2)], 2).is_ok());
        assert!(check_rows(&[row(3)], 0).is_ok());
        assert!(check_rows(&[row(2), row(1)], 2).is_err());
    }
}
