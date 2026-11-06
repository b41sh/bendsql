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

//! The requests to send and the responses to hand back, independent of any HTTP client.

use std::fmt;

/// Header names, in lower case (HTTP header names are case-insensitive).
pub const AUTHORIZATION: &str = "authorization";
pub const CONTENT_TYPE: &str = "content-type";
pub const ACCEPT: &str = "accept";
pub const COOKIE: &str = "cookie";
pub const SET_COOKIE: &str = "set-cookie";
pub const HEADER_QUERY_ID: &str = "x-databend-query-id";
pub const HEADER_TENANT: &str = "x-databend-tenant";
pub const HEADER_WAREHOUSE: &str = "x-databend-warehouse";
pub const HEADER_STICKY_NODE: &str = "x-databend-sticky-node";
pub const HEADER_ROUTE_HINT: &str = "x-databend-route-hint";
pub const HEADER_SESSION_ID: &str = "x-databend-session-id";

pub(crate) const CONTENT_TYPE_JSON: &str = "application/json";

/// The HTTP methods the protocol uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

/// A request to send. `Debug` hides the `authorization` and `cookie` values.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    /// The configured endpoint joined with a checked path, such as
    /// `https://host:8000/v1/query/0193…/page/1`.
    pub url: String,
    /// Header names are lower case; values contain no control characters.
    pub headers: Vec<(&'static str, String)>,
    /// A JSON body, for a POST that has one.
    pub body: Option<Vec<u8>>,
    /// Whether sending it again is harmless (pages, final, kill, login, refresh): a transport
    /// may retry these on a connection error. Starting a query is not: a connection error after
    /// it was sent leaves unknown whether the statement ran.
    pub replayable: bool,
}

impl HttpRequest {
    /// The value of a header, if the request has it.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(n, v)| {
                (
                    *n,
                    if is_secret_header(n) {
                        "***"
                    } else {
                        v.as_str()
                    },
                )
            })
            .collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .field("replayable", &self.replayable)
            .finish()
    }
}

/// A response, as received. `Debug` hides `set-cookie` values and shows the body's size.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Every header line as received; a name may repeat (`set-cookie`).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The first value of a header.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every value of a header, in order.
    pub fn headers_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(n, v)| {
                let v = if is_secret_header(n) {
                    "***"
                } else {
                    v.as_str()
                };
                (n.as_str(), v)
            })
            .collect();
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &headers)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

fn is_secret_header(name: &str) -> bool {
    [AUTHORIZATION, COOKIE, SET_COOKIE]
        .iter()
        .any(|s| name.eq_ignore_ascii_case(s))
}

/// Whether text can be sent as a header value: visible ASCII and spaces, nothing that could end
/// the header line or start another.
pub(crate) fn is_header_value(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_credentials_and_cookies() {
        let request = HttpRequest {
            method: Method::Post,
            url: "http://localhost:8000/v1/query".into(),
            headers: vec![
                (AUTHORIZATION, "Basic cm9vdDpzZWNyZXQ=".into()),
                (COOKIE, "session=abc".into()),
                (HEADER_TENANT, "t1".into()),
            ],
            body: Some(b"{}".to_vec()),
            replayable: false,
        };
        let text = format!("{request:?}");
        assert!(!text.contains("cm9vdDpzZWNyZXQ="));
        assert!(!text.contains("session=abc"));
        assert!(text.contains("t1"));

        let response = HttpResponse::new(200, "secret body").with_header("Set-Cookie", "k=v");
        let text = format!("{response:?}");
        assert!(!text.contains("k=v"));
        assert!(!text.contains("secret body"));
    }

    #[test]
    fn header_values_reject_line_breaks() {
        assert!(is_header_value("warehouse-1"));
        assert!(!is_header_value("a\r\nx-evil: 1"));
        assert!(!is_header_value("caf\u{e9}"));
    }

    #[test]
    fn response_headers_are_case_insensitive() {
        let response = HttpResponse::new(200, "")
            .with_header("Set-Cookie", "a=1")
            .with_header("set-cookie", "b=2");
        assert_eq!(response.headers_named(SET_COOKIE).count(), 2);
        assert_eq!(response.header("SET-COOKIE"), Some("a=1"));
    }
}
