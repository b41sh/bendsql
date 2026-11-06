# `databend-protocol`: the HTTP query protocol without I/O

Status: step 1 (the crate, `protocol/`) is written; `databend-client` doesn't use it yet.

Decided in review: the name is `databend-protocol`; logging in is the caller's choice; results
are read in the JSON format only; heartbeats and their timing stay in `databend-client`.

## Why

`databend-client` is the only way to speak Databend's HTTP query API from Rust, and it brings
reqwest, hyper, tower, tokio, every login method (JWT key pairs with PKCS#8, Cloud tokens),
stage uploads and presigning with it. `databend-driver` adds the SQL parser (for client-side
parameters), the full `arrow` crate and jsonb. An application that embeds Databend for queries
only (RustGIS reads spatial tables with it) gets about 130 extra crates, against 16 to 20 for
the PostgreSQL, MySQL and SQL Server clients, and Cargo's feature unification turned on
serde_json features (`arbitrary_precision`) that broke the application's own JSON.

`databend-protocol` holds what every client of `/v1/query` needs and nothing that performs I/O:
the request and response types, the session that travels between requests, the paging state
machine, the headers, the error codes, and decoding of the JSON result format. The caller sends
the requests it builds with whatever HTTP client it has (reqwest, ureq, a browser's `fetch`) and
hands back what came. `databend-client` becomes this crate plus a reqwest transport, with its
public API unchanged.

## Scope

In:

- `POST /v1/query`, its pages (`next_uri`), `final_uri` and `kill_uri`;
- `POST /v1/session/login` (optional), session tokens and their refresh;
- the session state the server returns and expects back (database, role, settings,
  transaction state, sticky node, the fields a client must echo without understanding);
- the headers: tenant, warehouse, route hint, sticky node, query id;
- cookies the server sets, sent back on later requests;
- the JSON result format: schema, rows of `Option<String>`, result format settings, and helpers
  to decode binary (hex, base64) and geometry cells;
- error responses and the codes clients act on (login refused, token expired, query aborted).

Out (stays in `databend-client` / `databend-driver`):

- any HTTP client, async runtime, retry timing or connection pool;
- DSN parsing (it is entangled with Cloud and key-pair options);
- Arrow results: the crate asks for and reads JSON pages only;
- typed values with chrono and jsonb (`databend-driver-core`);
- key-pair JWTs, Cloud WebAPI tokens, stage upload, presign, streaming load;
- heartbeats for result sets read slowly and for `need_keep_alive` sessions, and when to send
  them (a client that reads its pages straight away needs none).

## Dependencies

`serde` (derive), `serde_json` (`default-features = false, features = ["std"]`), `base64`.
No `url`: next and final URIs are paths, checked and joined by the crate (see *Security*).
It builds for `wasm32-unknown-unknown`. `#![forbid(unsafe_code)]`, and no `unwrap`, `expect` or
`panic!` outside tests: everything the server sends is decoded as untrusted input.

## Modules

| Module | Holds | Moved from |
|---|---|---|
| `http` | `HttpRequest`, `HttpResponse`, `Method`, header names | new (headers from `core/client.rs`) |
| `config` | `ClientConfig`, `Auth`, `SensitiveString` | new (fields from `APIClient`; `SensitiveString` from `core/auth.rs`) |
| `capability` | `Capability` from a server version, without semver | `core/capability.rs` |
| `session` | `SessionState` | `core/session.rs` |
| `request` | `QueryRequest`, `PaginationConfig`, `StageAttachmentConfig`, `LoginRequest`, `RefreshSessionTokenRequest` | `core/request.rs`, `core/login.rs` |
| `response` | `QueryResponse`, `QueryStats`, `SchemaField`, `LoginResponse`, `SessionTokenInfo`, `RefreshResponse` | `core/response.rs`, `core/login.rs` |
| `error` | `Error`, `ErrorCode`, code constants | `core/error_code.rs`, part of `core/error.rs` |
| `settings` | `QueryResultFormatSettings`, `GeometryDataType`, `BinaryFormat` | `core/settings.rs` (timezone stays a string; chrono-tz parsing stays in core) |
| `schema` | `DataType` parsed from the type names of a schema (`FromStr`) | the string half of `core/schema.rs`; `Field`, `Schema` and the Arrow conversions stay |
| `cell` | decoding helpers for JSON cells: binary, geometry, hex | new (the hex/base64 part of `sql/value/string_decoder.rs`) |
| `client` | `Client`: the session, cookies, auth and node a connection carries; builds requests | the request building of `APIClient` |
| `query` | `Query`, `Step`, `Next`, `QueryOptions`: one statement's paging | `core/pages.rs` without the `Stream` |
| `blocking` | `Transport`, `query_all`, `QueryResult`, `QueryError`: a statement run to its end | new (the loop RustGIS had) |

## The API

The rustdoc of `protocol/src` is the reference; this is its outline.

```rust
// config
pub enum Auth { Basic { user: String, password: SensitiveString }, Bearer(SensitiveString) }
pub struct ClientConfig {            // #[non_exhaustive]: build it with `new` and `with_*`
    pub endpoint: String,            // http(s)://host:port, nothing more
    pub auth: Auth,
    pub tenant: Option<String>,
    pub warehouse: Option<String>,
    pub session: SessionState,       // database, catalog, role, settings to start with
    pub pagination: Option<PaginationConfig>,
    pub session_tokens: bool,        // ask a login for session tokens (default true)
    pub max_body_bytes: usize,       // default 256 MiB
}

// client: one connection's state; no locking inside.
impl Client {
    pub fn new(config: ClientConfig, seed: u64) -> Result<Client>;   // seed: ids, route hints
    pub fn login_request(&self) -> Result<HttpRequest>;              // optional
    pub fn on_login(&mut self, response: HttpResponse) -> Result<LoginInfo>;
    pub fn refresh_request(&self) -> Result<Option<HttpRequest>>;    // after TokenExpired
    pub fn on_refresh(&mut self, response: HttpResponse) -> Result<()>;
    pub fn logout_request(&self) -> Option<HttpRequest>;             // tokens or keep-alive
    pub fn start_query(&mut self, sql: &str, options: QueryOptions) -> Result<(Query, HttpRequest)>;
    pub fn session(&self) -> &SessionState;
    pub fn session_token_ttl_secs(&self) -> Option<u64>;
    pub fn max_body_bytes(&self) -> usize;
}

// query: one statement, a response at a time.
impl Query {
    pub fn on_response(&mut self, client: &mut Client, response: HttpResponse) -> Result<Step>;
    pub fn retry_request(&self, client: &Client) -> Option<HttpRequest>; // after is_retryable()
    pub fn kill_request(&self, client: &Client) -> Option<HttpRequest>;  // until it is finished
    pub fn id(&self) -> &str;
    pub fn schema(&self) -> &[SchemaField];
    pub fn settings(&self) -> &QueryResultFormatSettings;
    pub fn is_finished(&self) -> bool;
}
pub struct Step { pub rows: Vec<Vec<Option<String>>>, pub stats: QueryStats, pub warnings: Vec<String>, pub next: Next }
pub enum Next { Page(HttpRequest), Final(HttpRequest), Done }
```

Paging follows the server's `next_uri`: pages (or the query's state while it runs), then
`…/final`, whose response ends the query and releases its result. Pages carry the node that ran
the query (`x-databend-sticky-node`); a statement starts on the last node when the session needs
it (`need_sticky`), and keeps the route hint inside a transaction.

## Errors

`Error` is `Config` (bad settings), `Login` (401 or a refused login), `TokenExpired` (401 with
5101 or 5103 while using a session token: refresh, then `retry_request`), `Query` (the statement
failed), `QueryNotFound` (a 404 after the first response), `Http` (another status, with the
server's `ErrorCode` when the body had one) and `Protocol` (a response that isn't the
protocol's). `is_retryable()` is true for `TokenExpired` and a 503. `kind()` says what an error means
from its status and the server's code: `Login` (2201, 2215, 5100), `Aborted` (1043: past
`max_execute_time_in_seconds`, or killed), `Lost` (5101 to 5104, a lost page, a 5xx: the
statement may succeed on a new session), `Statement`, `Protocol` or `Config`. No error holds a password,
a token or an `authorization` header, and JSON that fails to decode is reported by position, not
quoted, since a login's body holds tokens.

## Security

- **Next, final and kill URIs stay on the endpoint.** Only a path under `/v1/query/` of letters,
  digits, `-`, `_`, `.` and `/` is accepted (no empty, `.` or `..` segment, no query), and it is
  joined to the configured endpoint. Anything else is `Error::Protocol`.
- **Bounded input.** A body over `max_body_bytes` is refused; JSON keeps serde_json's recursion
  limit; type names nest at most 64 deep; ids are at most 128 characters; at most 32 cookies of
  4 KiB are kept.
- **Header values** (tenant, warehouse, node id, route hint, tokens, cookies) must be visible
  ASCII, so nothing the server sends can add a header line.
- **Secrets.** Secrets and tokens are `SensitiveString`; `Debug` of `HttpRequest`,
  `HttpResponse` and `Client` hides `authorization`, cookies and tokens.

## Using it

The loop in the crate's documentation (`protocol/src/lib.rs`): start a query, send each request,
hand its response to `on_response`, keep the rows, follow `Next` until `Done`.

A blocking caller can leave that to `query_all` (module `blocking`), with a `Transport` that
sends requests (`send`, and `send_kill` for the kill after a failure) and waits (`wait`). It
pages through the result, waits out a 503 and refreshes an expired session token (at most
three times a request), kills the statement when it fails, and returns a `QueryResult` (id,
schema, rows, settings, stats, warnings). It doesn't run a statement again after a lost
session: whether that is safe depends on the statement. The transport bounds how long requests
take; the crate reads no clock.

`ClientConfig::max_rows` caps the rows a statement may return over all its pages (no cap by
default). `cell::boolean`, `int`, `uint` and `float` read the JSON format's cells (booleans
are `1` and `0`), next to `cell::binary` and `cell::geometry`.

## Changes to the other crates

1. **`databend-protocol`** (this step): the crate, its types copied from core with their tests,
   and tests of the state machine against recorded responses. Nothing else changes.
2. **`databend-client`** depends on it and re-exports the moved types under their current paths
   (`SchemaField`, `QueryStats`, `BinaryFormat`, `GeometryDataType`, `QueryResultFormatSettings`,
   `SensitiveString`, `schema::DataType`…). Its request building and paging go through `Client`
   and `Query`; reqwest, retries, heartbeats and their timing stay. Arrow results
   (`query_result_format=arrow`, not the default) keep their own path in core, since the
   protocol crate reads JSON only. Key-pair login and Cloud WebAPI move behind default features.
3. **`databend-driver`**: the SQL parser behind a default `sql-parser` feature.
4. **`databend-driver-core`**: `arrow-array` instead of `arrow`.

Behaviour differences from core that step 2 has to keep in mind:

- `FromStr` of `GeometryDataType`, `BinaryFormat` and `DataType` fails with
  `databend_protocol::Error`; core needs `From<databend_protocol::Error>` for its `Error`.
- `Nullable(String NULL)` parses to `Nullable(String)` (core makes it nullable twice), and
  `Display` of a `Map` that isn't a pair no longer panics.
- The final URI is followed from `next_uri`, as the server sends it, rather than built from the
  query id; kill uses the server's `kill_uri` when there is one.

## RustGIS

RustGIS depends on `databend-protocol` alone and sends its requests with the ureq agent it
already has (platform certificate verification, `tls_ca_file` as an extra root). Its Databend
provider stops needing tokio, reqwest, the driver and the client, about 125 crates. The SQL it
builds and the way it reads tables don't change.
