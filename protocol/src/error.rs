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

use std::fmt::{self, Display, Formatter};

use serde::Deserialize;

const SESSION_TOKEN_EXPIRED: u16 = 5101;
const SESSION_TOKEN_NOT_FOUND: u16 = 5103;

/// The most of an unexpected body an error message quotes.
const MAX_QUOTED_BODY_CHARS: usize = 512;

/// Whether a 401 with this code asks for a session token refresh.
pub fn need_refresh_token(code: u16) -> bool {
    code == SESSION_TOKEN_EXPIRED || code == SESSION_TOKEN_NOT_FOUND
}

/// An error the server reports.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ErrorCode {
    pub code: u16,
    pub message: String,
    pub detail: Option<String>,
}

impl Display for ErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(d) if !d.is_empty() => write!(f, "[{}]{}\n{}", self.code, self.message, d),
            _ => write!(f, "[{}]{}", self.code, self.message),
        }
    }
}

/// The body of a response whose status isn't 200.
#[derive(Deserialize, Debug)]
pub struct ResponseWithErrorCode {
    pub error: ErrorCode,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What can go wrong. No variant ever holds a password, a token or an `authorization` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The settings can't make requests: a bad endpoint, a header value with a line break.
    Config(String),
    /// The server refused the credentials (401).
    Login(ErrorCode),
    /// The session token expired or is unknown (401 with 5101 or 5103): refresh it, then send
    /// the request again (`Query::retry_request`).
    TokenExpired(ErrorCode),
    /// The statement failed on the server.
    Query(ErrorCode),
    /// A page the server no longer has (404 after the first response): the result expired,
    /// or the request reached another node.
    QueryNotFound(String),
    /// Another status, with the server's error when the body had one, else the start of the body.
    Http {
        status: u16,
        error: Option<ErrorCode>,
        body: String,
    },
    /// A response that isn't the protocol's: bad JSON, too large, a next URI off the endpoint.
    Protocol(String),
}

impl Error {
    /// The server's error code, when it gave one.
    pub fn code(&self) -> Option<u16> {
        match self {
            Error::Login(e) | Error::TokenExpired(e) | Error::Query(e) => Some(e.code),
            Error::Http { error, .. } => error.as_ref().map(|e| e.code),
            Error::Config(_) | Error::QueryNotFound(_) | Error::Protocol(_) => None,
        }
    }

    /// The HTTP status, for errors that came with one other than 200.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Http { status, .. } => Some(*status),
            Error::Login(_) | Error::TokenExpired(_) => Some(401),
            Error::QueryNotFound(_) => Some(404),
            Error::Config(_) | Error::Query(_) | Error::Protocol(_) => None,
        }
    }

    /// Whether sending the same request again may succeed: a 503 (the server is starting or
    /// busy) or an expired session token (after a refresh).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::TokenExpired(_) | Error::Http { status: 503, .. }
        )
    }

    /// The error for a status other than 200. `session_tokens` says whether the request used a
    /// session token, the only case where a 401 asks for a refresh.
    pub(crate) fn from_status(status: u16, body: &[u8], session_tokens: bool) -> Error {
        let error = serde_json::from_slice::<ResponseWithErrorCode>(body)
            .ok()
            .map(|r| r.error);
        match (status, error) {
            (401, Some(e)) if session_tokens && need_refresh_token(e.code) => {
                Error::TokenExpired(e)
            }
            (401, Some(e)) => Error::Login(e),
            (401, None) => Error::Login(ErrorCode {
                code: 0,
                message: quote_body(body),
                detail: None,
            }),
            (status, error) => Error::Http {
                status,
                error,
                body: quote_body(body),
            },
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(msg) => write!(f, "invalid client settings: {msg}"),
            Error::Login(e) => write!(f, "login refused: {e}"),
            Error::TokenExpired(e) => write!(f, "session token expired: {e}"),
            Error::Query(e) => write!(f, "query failed: {e}"),
            Error::QueryNotFound(msg) => write!(f, "query not found: {msg}"),
            Error::Http {
                status,
                error: Some(e),
                ..
            } => write!(f, "HTTP {status}: {e}"),
            Error::Http { status, body, .. } => write!(f, "HTTP {status}: {body}"),
            Error::Protocol(msg) => write!(f, "unexpected response: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// The start of a body, as text with control characters replaced, for an error message.
pub(crate) fn quote_body(body: &[u8]) -> String {
    // at most four bytes a character: no need to look further
    let head = body.get(..MAX_QUOTED_BODY_CHARS * 4).unwrap_or(body);
    let text = String::from_utf8_lossy(head);
    let mut out: String = text
        .chars()
        .take(MAX_QUOTED_BODY_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if head.len() < body.len() || text.chars().nth(MAX_QUOTED_BODY_CHARS).is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_display() {
        let e = ErrorCode {
            code: 1025,
            message: "Unknown table".into(),
            detail: Some("t1".into()),
        };
        assert_eq!(e.to_string(), "[1025]Unknown table\nt1");
    }

    #[test]
    fn statuses_become_errors() {
        let expired = br#"{"error":{"code":5101,"message":"expired"}}"#;
        assert!(matches!(
            Error::from_status(401, expired, true),
            Error::TokenExpired(e) if e.code == 5101
        ));
        // without session tokens there is nothing to refresh
        assert!(matches!(
            Error::from_status(401, expired, false),
            Error::Login(_)
        ));
        assert!(matches!(
            Error::from_status(401, b"no json", true),
            Error::Login(_)
        ));
        let e = Error::from_status(503, b"starting", false);
        assert!(e.is_retryable());
        assert_eq!(e.status(), Some(503));
        let e = Error::from_status(400, br#"{"error":{"code":1005,"message":"syntax"}}"#, false);
        assert_eq!(e.code(), Some(1005));
    }

    #[test]
    fn quoted_bodies_are_short_and_on_one_line() {
        assert_eq!(quote_body(b"a\r\nb"), "a  b");
        let long = vec![b'x'; 10_000];
        let quoted = quote_body(&long);
        assert_eq!(quoted.chars().count(), MAX_QUOTED_BODY_CHARS + 1);
        assert!(quoted.ends_with('…'));
        assert_eq!(quote_body(&[0xff, b'a']), "\u{fffd}a");
    }
}
