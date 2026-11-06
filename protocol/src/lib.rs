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

//! Databend's HTTP query protocol without I/O.
//!
//! The crate builds the requests a client sends (`POST /v1/query`, its pages, final and kill,
//! and optionally the session login, refresh and logout) and reads the responses the caller
//! hands back. Sending them is the caller's job, with any HTTP client: it decides timeouts, TLS,
//! retries and threads. Results are read in Databend's JSON format only.
//!
//! ```no_run
//! use databend_protocol::{Auth, Client, ClientConfig, HttpRequest, HttpResponse, Next, QueryOptions};
//!
//! # fn send(_: HttpRequest) -> HttpResponse { unimplemented!() }
//! # fn main() -> Result<(), databend_protocol::Error> {
//! let auth = Auth::basic("root", "");
//! let mut client = Client::new(ClientConfig::new("http://localhost:8000", auth), 42)?;
//! let (mut query, mut request) = client.start_query("SELECT 1", QueryOptions::default())?;
//! let mut rows = Vec::new();
//! loop {
//!     let step = query.on_response(&mut client, send(request))?;
//!     rows.extend(step.rows);
//!     match step.next {
//!         Next::Page(next) | Next::Final(next) => request = next,
//!         Next::Done => break,
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! A blocking caller can leave the loop, the retries and the kill to [`query_all`], with a
//! [`Transport`] that sends the requests:
//!
//! ```no_run
//! use std::time::Duration;
//! use databend_protocol::{query_all, Auth, Client, ClientConfig, QueryOptions, Transport};
//! use databend_protocol::{HttpRequest, HttpResponse};
//!
//! struct MyHttp;
//! impl Transport for MyHttp {
//!     type Error = std::io::Error;
//!     fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, Self::Error> {
//!         unimplemented!("send it with any HTTP client")
//!     }
//!     fn wait(&mut self, delay: Duration) {
//!         std::thread::sleep(delay)
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let config = ClientConfig::new("http://localhost:8000", Auth::basic("root", ""));
//! let mut client = Client::new(config, 42)?;
//! let result = query_all(&mut client, &mut MyHttp, "SELECT 1", QueryOptions::default())?;
//! assert_eq!(result.rows.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! Everything the server sends is untrusted: bodies are size-limited, next URIs must stay on the
//! configured endpoint, and nothing here panics on malformed input.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::indexing_slicing
    )
)]

pub mod blocking;
mod capability;
pub mod cell;
mod client;
mod config;
mod error;
pub mod http;
mod query;
mod request;
mod response;
pub mod schema;
mod session;
mod settings;

pub use blocking::{query_all, QueryError, QueryResult, Transport};
pub use capability::Capability;
pub use client::{Client, LoginInfo};
pub use config::{Auth, ClientConfig, Pagination, SensitiveString};
pub use error::{need_refresh_token, Error, ErrorCode, ErrorKind, ResponseWithErrorCode, Result};
pub use http::{HttpRequest, HttpResponse, Method};
pub use query::{Next, Query, QueryOptions, Step};
pub use request::{
    LoginRequest, PaginationConfig, QueryRequest, RefreshSessionTokenRequest, StageAttachmentConfig,
};
pub use response::{
    LoginResponse, LoginResponseResult, ProgressValues, Progresses, QueryResponse, QueryStats,
    RefreshResponse, SchemaField, SessionTokenInfo, SpillProgress,
};
pub use session::SessionState;
pub use settings::{BinaryFormat, GeometryDataType, QueryResultFormatSettings};

#[cfg(test)]
mod tests;
