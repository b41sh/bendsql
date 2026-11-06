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

//! A blocking driver for the state machine: given a way to send requests, a statement's rows.
//!
//! The caller still does all the I/O, through [`Transport`]: it decides timeouts, TLS and how to
//! wait. [`query_all`] does what every blocking caller would otherwise write: page through the
//! result, send the request again after a 503 or an expired session token, and stop the
//! statement on the server when it fails.

use std::fmt;
use std::time::Duration;

use crate::client::Client;
use crate::error::Error;
use crate::http::{HttpRequest, HttpResponse};
use crate::query::{Next, Query, QueryOptions};
use crate::response::{QueryStats, SchemaField};
use crate::settings::QueryResultFormatSettings;

/// How many times one request is sent again after a 503 or an expired session token.
pub const MAX_RETRIES: u32 = 3;

/// The wait before the first resend after a 503; each further one waits this much longer.
pub const RETRY_DELAY: Duration = Duration::from_millis(500);

/// How requests reach the server.
pub trait Transport {
    /// Why a request got no response: no connection, a reset, a timeout.
    type Error;

    /// Sends a request and returns the server's response, whatever its status. A transport
    /// should stop reading a body past `Client::max_body_bytes` (the client refuses it anyway)
    /// and bound how long a request may take: a server can keep a query running for ever.
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, Self::Error>;

    /// Sends a kill request after a failure, ignoring how it goes. A transport whose requests
    /// share a deadline gives this one a few seconds of its own: it often follows a timeout.
    fn send_kill(&mut self, request: HttpRequest) {
        let _ = self.send(request);
    }

    /// Waits before a request is sent again (a 503: the server is starting or busy).
    fn wait(&mut self, delay: Duration);
}

/// A statement's whole result.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    /// The server's id for the statement.
    pub id: String,
    pub schema: Vec<SchemaField>,
    /// A cell of text per column of `schema` (see [`crate::cell`]); `None` is SQL `NULL`.
    pub rows: Vec<Vec<Option<String>>>,
    /// How the cells are encoded (binary and geometry formats, time zone).
    pub settings: QueryResultFormatSettings,
    /// The progress the last response reported.
    pub stats: QueryStats,
    /// The server's warnings.
    pub warnings: Vec<String>,
}

/// Why [`query_all`] failed.
#[derive(Debug)]
pub enum QueryError<E> {
    /// What the protocol made of the settings or the server's answers.
    Databend(Error),
    /// A request got no response.
    Transport(E),
}

impl<E> QueryError<E> {
    /// The protocol's error, unless a request got no response.
    pub fn databend(&self) -> Option<&Error> {
        match self {
            QueryError::Databend(e) => Some(e),
            QueryError::Transport(_) => None,
        }
    }
}

impl<E: fmt::Display> fmt::Display for QueryError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::Databend(e) => e.fmt(f),
            QueryError::Transport(e) => e.fmt(f),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for QueryError<E> {}

/// Runs a statement to its end and returns every row.
///
/// Pages are fetched one after another. A 503 is waited out and an expired session token
/// refreshed, at most [`MAX_RETRIES`] times a request. When the statement fails, a request gets
/// no response or the rows pass `ClientConfig::max_rows`, the statement is killed on the server
/// and the error returned. Once every row has come, a final request that fails doesn't fail the
/// statement: the server releases its result when it expires.
///
/// The statement isn't run again after a lost session (`ErrorKind::Lost`): whether that is
/// safe depends on the statement, so it is the caller's call.
pub fn query_all<T: Transport>(
    client: &mut Client,
    transport: &mut T,
    sql: &str,
    options: QueryOptions,
) -> Result<QueryResult, QueryError<T::Error>> {
    let (mut query, mut request) = client
        .start_query(sql, options)
        .map_err(QueryError::Databend)?;
    let mut result = QueryResult::default();
    let mut retries = 0u32;
    // whether the request is the final one: every row has come
    let mut finishing = false;
    let failure = loop {
        let response = match transport.send(request) {
            Ok(response) => response,
            Err(_) if finishing => return Ok(finish(result, &query)),
            Err(e) => break QueryError::Transport(e),
        };
        match query.on_response(client, response) {
            Ok(step) => {
                retries = 0;
                result.rows.extend(step.rows);
                result.warnings.extend(step.warnings);
                // the final response holds no progress worth keeping
                if !finishing {
                    result.stats = step.stats;
                }
                match step.next {
                    Next::Page(next) => request = next,
                    Next::Final(next) => {
                        finishing = true;
                        request = next;
                    }
                    Next::Done => return Ok(finish(result, &query)),
                }
            }
            Err(_) if finishing => return Ok(finish(result, &query)),
            Err(e) if e.is_retryable() && retries < MAX_RETRIES => {
                retries += 1;
                if matches!(e, Error::TokenExpired(_)) {
                    if let Err(f) = refresh(client, transport) {
                        break f;
                    }
                } else {
                    transport.wait(RETRY_DELAY.saturating_mul(retries));
                }
                match query.retry_request(client) {
                    Some(again) => request = again,
                    None => break QueryError::Databend(e),
                }
            }
            Err(e) => break QueryError::Databend(e),
        }
    };
    // the server would otherwise keep the statement until it times out
    if let Some(kill) = query.kill_request(client) {
        transport.send_kill(kill);
    }
    Err(failure)
}

fn finish(mut result: QueryResult, query: &Query) -> QueryResult {
    query.id().clone_into(&mut result.id);
    result.schema = query.schema().to_vec();
    result.settings = query.settings().clone();
    result
}

/// New session tokens, after one expired.
fn refresh<T: Transport>(
    client: &mut Client,
    transport: &mut T,
) -> Result<(), QueryError<T::Error>> {
    let Some(request) = client.refresh_request().map_err(QueryError::Databend)? else {
        return Ok(());
    };
    let response = transport.send(request).map_err(QueryError::Transport)?;
    client.on_refresh(response).map_err(QueryError::Databend)
}
