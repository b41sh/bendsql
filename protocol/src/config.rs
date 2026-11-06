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

use std::fmt;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;

use crate::error::{Error, Result};
use crate::http::is_header_value;
use crate::request::PaginationConfig;
use crate::session::SessionState;

/// The largest response body read by default: 256 MiB.
pub const DEFAULT_MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

/// Text that `Debug` and `Display` hide: passwords and tokens.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(from = "String", into = "String")]
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SensitiveString(String);

impl From<String> for SensitiveString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SensitiveString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<SensitiveString> for String {
    fn from(value: SensitiveString) -> Self {
        value.0
    }
}

impl fmt::Display for SensitiveString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "**REDACTED**")
    }
}

impl fmt::Debug for SensitiveString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the double quotes keep the look of a `String`
        write!(f, "\"**REDACTED**\"")
    }
}

impl SensitiveString {
    #[must_use]
    pub fn inner(&self) -> &str {
        self.0.as_str()
    }
}

/// How requests authenticate before a login gives session tokens, or without a login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// HTTP basic authentication.
    Basic {
        user: String,
        password: SensitiveString,
    },
    /// A bearer token obtained some other way (an access token).
    Bearer(SensitiveString),
}

impl Auth {
    pub fn basic(user: impl Into<String>, password: impl Into<SensitiveString>) -> Self {
        Auth::Basic {
            user: user.into(),
            password: password.into(),
        }
    }

    pub fn bearer(token: impl Into<SensitiveString>) -> Self {
        Auth::Bearer(token.into())
    }

    /// The `authorization` header value.
    pub(crate) fn header_value(&self) -> String {
        match self {
            Auth::Basic { user, password } => {
                let pair = format!("{user}:{}", password.inner());
                format!("Basic {}", BASE64_STANDARD.encode(pair))
            }
            Auth::Bearer(token) => format!("Bearer {}", token.inner()),
        }
    }

    fn validate(&self) -> Result<()> {
        let ok = match self {
            // the user name may not hold `:`, which separates it from the password
            Auth::Basic { user, .. } => !user.contains(':') && !user.chars().any(char::is_control),
            Auth::Bearer(token) => is_header_value(token.inner()),
        };
        if ok {
            Ok(())
        } else {
            Err(Error::Config(
                "the user name or token holds characters a request header can't carry".into(),
            ))
        }
    }
}

/// The settings of one connection.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ClientConfig {
    /// `http://host:port` or `https://host:port`, with no path, query or user name.
    pub endpoint: String,
    pub auth: Auth,
    /// `X-DATABEND-TENANT`, for servers serving several tenants.
    pub tenant: Option<String>,
    /// `X-DATABEND-WAREHOUSE`; a `warehouse` setting the server returns replaces it.
    pub warehouse: Option<String>,
    /// The session to start with: database, catalog, role, settings.
    pub session: SessionState,
    /// Sent with every query that starts; `None` leaves the server's defaults.
    pub pagination: Option<Pagination>,
    /// Ask for session tokens at login (the default). Without a login, or with false, every
    /// request carries `auth`.
    pub session_tokens: bool,
    /// The largest response body read; a larger one is an error.
    pub max_body_bytes: usize,
}

/// How the server pages results: how long a page waits for rows, and how many rows it holds.
pub type Pagination = PaginationConfig;

impl ClientConfig {
    pub fn new(endpoint: impl Into<String>, auth: Auth) -> Self {
        Self {
            endpoint: endpoint.into(),
            auth,
            tenant: None,
            warehouse: None,
            session: SessionState::default(),
            pagination: None,
            session_tokens: true,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }

    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    pub fn with_warehouse(mut self, warehouse: impl Into<String>) -> Self {
        self.warehouse = Some(warehouse.into());
        self
    }

    pub fn with_session(mut self, session: SessionState) -> Self {
        self.session = session;
        self
    }

    pub fn with_pagination(mut self, pagination: Pagination) -> Self {
        self.pagination = Some(pagination);
        self
    }

    pub fn with_session_tokens(mut self, session_tokens: bool) -> Self {
        self.session_tokens = session_tokens;
        self
    }

    pub fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }

    /// Checks the settings and returns the endpoint without a trailing `/`.
    pub(crate) fn validate(&self) -> Result<String> {
        let endpoint = normalize_endpoint(&self.endpoint)?;
        self.auth.validate()?;
        for (what, value) in [("tenant", &self.tenant), ("warehouse", &self.warehouse)] {
            if let Some(value) = value {
                if value.is_empty() || !is_header_value(value) {
                    return Err(Error::Config(format!(
                        "the {what} must be non-empty visible ASCII text"
                    )));
                }
            }
        }
        if self.max_body_bytes == 0 {
            return Err(Error::Config("max_body_bytes must be above 0".into()));
        }
        Ok(endpoint)
    }
}

/// `http(s)://host[:port]`, with an optional trailing `/`, and nothing else.
fn normalize_endpoint(endpoint: &str) -> Result<String> {
    // the endpoint itself stays out of the message: it might hold a password
    let bad = |why: &str| Error::Config(format!("the endpoint {why}"));
    let trimmed = endpoint.strip_suffix('/').unwrap_or(endpoint);
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .ok_or_else(|| bad("must start with http:// or https://"))?;
    if rest.is_empty() {
        return Err(bad("has no host"));
    }
    if rest.contains('@') {
        return Err(bad("may not hold a user name or password"));
    }
    if rest.contains(['/', '?', '#', '\\']) {
        return Err(bad("may not hold a path, query or fragment"));
    }
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']'))
    {
        return Err(bad("holds characters a host name can't"));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_string_hides_its_content() {
        let value = SensitiveString::from("hello world");
        assert_eq!(format!("{value}"), "**REDACTED**");
        assert_eq!(format!("{value:?}"), "\"**REDACTED**\"");
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, "\"hello world\"");
        let back: SensitiveString = serde_json::from_str(&json).unwrap();
        assert_eq!(back.inner(), "hello world");
    }

    #[test]
    fn auth_headers() {
        // RFC 7617's example
        let basic = Auth::basic("Aladdin", "open sesame");
        assert_eq!(basic.header_value(), "Basic cm9vdDpzZWNyZXQ=");
        assert!(!format!("{basic:?}").contains("open sesame"));
        assert_eq!(Auth::bearer("tok").header_value(), "Bearer tok");
    }

    #[test]
    fn endpoints() {
        let ok = |e: &str| normalize_endpoint(e).unwrap();
        assert_eq!(ok("http://localhost:8000"), "http://localhost:8000");
        assert_eq!(ok("https://db.example.com/"), "https://db.example.com");
        assert_eq!(ok("http://[::1]:8000"), "http://[::1]:8000");
        for bad in [
            "localhost:8000",
            "ftp://host",
            "http://",
            "http://user:pw@host",
            "http://host/v1",
            "http://host?x=1",
            "http://host:8000\r\nx: y",
        ] {
            assert!(normalize_endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_rejects_header_injection() {
        let config = ClientConfig::new("http://localhost:8000", Auth::basic("root", ""))
            .with_warehouse("w1\r\nx-evil: 1");
        assert!(matches!(config.validate(), Err(Error::Config(_))));
        let config = ClientConfig::new("http://localhost:8000", Auth::basic("a:b", ""));
        assert!(matches!(config.validate(), Err(Error::Config(_))));
    }
}
