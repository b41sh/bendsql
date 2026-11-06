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

//! The state machine against recorded responses: a whole query, the session it carries, and
//! what a hostile server can send.

use serde_json::{json, Value};

use crate::http::*;
use crate::*;

const ENDPOINT: &str = "http://db.example:8000";

fn client() -> Client {
    let auth = Auth::basic("root", "pw");
    Client::new(ClientConfig::new(ENDPOINT, auth), 7).unwrap()
}

fn ok(body: Value) -> HttpResponse {
    HttpResponse::new(200, body.to_string())
}

/// A page of query `q1`: `next` is the path that follows, if any.
fn page(data: Value, next: Option<&str>) -> Value {
    json!({
        "id": "q1",
        "node_id": "node-a",
        "session": {"database": "geo", "need_sticky": false},
        "schema": [{"name": "id", "type": "Int32"}, {"name": "name", "type": "Nullable(String)"}],
        "data": data,
        "state": "Succeeded",
        "settings": {"geometry_output_format": "WKB"},
        "error": null,
        "warnings": ["slow plan"],
        "next_uri": next,
        "final_uri": "/v1/query/q1/final",
        "kill_uri": "/v1/query/q1/kill",
    })
}

fn body_json(request: &HttpRequest) -> Value {
    serde_json::from_slice(request.body.as_deref().unwrap()).unwrap()
}

#[test]
fn a_query_from_start_to_final() {
    let mut client = client();
    let (mut query, start) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    assert_eq!(start.method, Method::Post);
    assert_eq!(start.url, format!("{ENDPOINT}/v1/query"));
    assert!(!start.replayable);
    assert_eq!(start.header(AUTHORIZATION), Some("Basic cm9vdDpwdw=="));
    assert_eq!(start.header(CONTENT_TYPE), Some("application/json"));
    assert_eq!(start.header(HEADER_QUERY_ID), Some(query.id()));
    assert_eq!(start.header(COOKIE), Some("cookie_enabled=true"));
    assert!(start
        .header(HEADER_ROUTE_HINT)
        .is_some_and(|h| h.starts_with("rh:")));
    assert!(start.header(HEADER_STICKY_NODE).is_none());
    assert_eq!(body_json(&start)["sql"], "SELECT 1");

    let first = ok(page(json!([["1", "a"]]), Some("/v1/query/q1/page/1")))
        .with_header("Set-Cookie", "affinity=xyz; Path=/; HttpOnly")
        .with_header("X-DATABEND-ROUTE-HINT", "rh:server:000001");
    let step = query.on_response(&mut client, first).unwrap();
    assert_eq!(step.rows, vec![vec![Some("1".into()), Some("a".into())]]);
    assert_eq!(step.warnings, vec!["slow plan".to_string()]);
    assert_eq!(query.id(), "q1");
    assert_eq!(query.node_id(), Some("node-a"));
    assert_eq!(query.schema().len(), 2);
    assert_eq!(
        query.settings().geometry_format().unwrap(),
        GeometryDataType::WKB
    );
    assert_eq!(client.session().database.as_deref(), Some("geo"));
    let Next::Page(next) = step.next else {
        panic!("expected a page")
    };
    assert_eq!(next.method, Method::Get);
    assert_eq!(next.url, format!("{ENDPOINT}/v1/query/q1/page/1"));
    assert!(next.replayable);
    assert_eq!(next.header(HEADER_QUERY_ID), Some("q1"));
    assert_eq!(next.header(HEADER_STICKY_NODE), Some("node-a"));
    assert_eq!(next.header(HEADER_ROUTE_HINT), Some("rh:server:000001"));
    assert_eq!(
        next.header(COOKIE),
        Some("affinity=xyz; cookie_enabled=true")
    );

    let second = ok(page(json!([["2", null]]), Some("/v1/query/q1/final")));
    let step = query.on_response(&mut client, second).unwrap();
    assert_eq!(step.rows, vec![vec![Some("2".into()), None]]);
    assert!(
        step.warnings.is_empty(),
        "warnings come with the first response"
    );
    let Next::Final(fin) = step.next else {
        panic!("expected the final request")
    };
    assert_eq!(fin.url, format!("{ENDPOINT}/v1/query/q1/final"));
    assert!(!query.is_finished());

    let last =
        ok(json!({"id": "q1", "schema": [], "data": [], "state": "Succeeded", "next_uri": null}));
    let step = query.on_response(&mut client, last).unwrap();
    assert!(matches!(step.next, Next::Done));
    assert!(query.is_finished());
    assert!(query.kill_request(&client).is_none());
    assert!(query.retry_request(&client).is_none());
    // the final response returns no session: the last one stays
    assert_eq!(client.session().database.as_deref(), Some("geo"));
}

#[test]
fn next_uris_off_the_endpoint_are_refused() {
    for next in [
        "http://evil.example/v1/query/q1/page/1",
        "//evil.example/v1/query/q1/page/1",
        "/v1/query/q1/../../session/logout",
        "/v1/session/logout",
    ] {
        let mut client = client();
        let (mut query, _) = client
            .start_query("SELECT 1", QueryOptions::default())
            .unwrap();
        let err = query
            .on_response(&mut client, ok(page(json!([]), Some(next))))
            .unwrap_err();
        assert!(matches!(err, Error::Protocol(_)), "{next}: {err:?}");
        assert!(query.retry_request(&client).is_none());
        // the query may still run: it can be stopped, on the endpoint
        let kill = query.kill_request(&client).unwrap();
        assert_eq!(kill.url, format!("{ENDPOINT}/v1/query/q1/kill"));
    }
}

#[test]
fn a_failed_statement_can_be_killed_on_its_node() {
    let mut client = client();
    let (mut query, _) = client
        .start_query("SELECT x", QueryOptions::default())
        .unwrap();
    let mut body = page(json!([]), Some("/v1/query/q1/final"));
    body["error"] = json!({"code": 1065, "message": "column x doesn't exist"});
    let err = query.on_response(&mut client, ok(body)).unwrap_err();
    assert_eq!(err.code(), Some(1065));
    assert!(matches!(err, Error::Query(_)));
    let kill = query.kill_request(&client).unwrap();
    assert_eq!(kill.method, Method::Post);
    assert_eq!(kill.header(HEADER_STICKY_NODE), Some("node-a"));
}

#[test]
fn a_lost_page_is_query_not_found() {
    let mut client = client();
    let (mut query, _) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    query
        .on_response(
            &mut client,
            ok(page(json!([]), Some("/v1/query/q1/page/1"))),
        )
        .unwrap();
    let gone = HttpResponse::new(404, r#"{"error":{"code":404,"message":"q1 expired"}}"#);
    let err = query.on_response(&mut client, gone).unwrap_err();
    assert_eq!(err, Error::QueryNotFound("q1 expired".into()));
    assert!(query.retry_request(&client).is_none());
}

#[test]
fn a_busy_server_gets_the_same_request_again() {
    let mut client = client();
    let (mut query, start) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    let err = query
        .on_response(&mut client, HttpResponse::new(503, "starting"))
        .unwrap_err();
    assert!(err.is_retryable());
    let again = query.retry_request(&client).unwrap();
    assert_eq!(again, start);
}

/// Logs in with session tokens `s1`/`r1`.
fn logged_in() -> Client {
    let mut client = client();
    let login = client.login_request().unwrap();
    assert_eq!(login.url, format!("{ENDPOINT}/v1/session/login"));
    assert!(login
        .header(AUTHORIZATION)
        .is_some_and(|a| a.starts_with("Basic ")));
    let info = client
        .on_login(
            ok(json!({"version": "1.2.901-nightly",
                      "tokens": {"session_token": "s1", "refresh_token": "r1",
                                 "session_token_ttl_in_secs": 600}}))
            .with_header("X-DATABEND-SESSION-ID", "sess-1"),
        )
        .unwrap();
    assert_eq!(info.version.as_deref(), Some("1.2.901-nightly"));
    assert!(info.capability.server_side_params);
    assert_eq!(client.session_id(), Some("sess-1"));
    assert_eq!(client.session_token_ttl_secs(), Some(600));
    client
}

#[test]
fn an_expired_session_token_is_refreshed_and_the_page_sent_again() {
    let mut client = logged_in();
    let (mut query, start) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    assert_eq!(start.header(AUTHORIZATION), Some("Bearer s1"));
    query
        .on_response(
            &mut client,
            ok(page(json!([]), Some("/v1/query/q1/page/1"))),
        )
        .unwrap();
    let expired = HttpResponse::new(401, r#"{"error":{"code":5101,"message":"expired"}}"#);
    let err = query.on_response(&mut client, expired).unwrap_err();
    assert!(matches!(err, Error::TokenExpired(_)));

    let refresh = client.refresh_request().unwrap().unwrap();
    assert_eq!(refresh.url, format!("{ENDPOINT}/v1/session/refresh"));
    assert_eq!(refresh.header(AUTHORIZATION), Some("Bearer r1"));
    assert_eq!(body_json(&refresh)["session_token"], "s1");
    client
        .on_refresh(ok(json!({"session_token": "s2", "refresh_token": "r2"})))
        .unwrap();

    let again = query.retry_request(&client).unwrap();
    assert_eq!(again.url, format!("{ENDPOINT}/v1/query/q1/page/1"));
    assert_eq!(again.header(AUTHORIZATION), Some("Bearer s2"));
}

#[test]
fn login_without_session_tokens_or_on_an_old_server() {
    let config = ClientConfig::new(ENDPOINT, Auth::basic("root", "pw")).with_session_tokens(false);
    let mut client = Client::new(config, 1).unwrap();
    let login = client.login_request().unwrap();
    assert!(login
        .url
        .ends_with("/v1/session/login?disable_session_token=true"));
    // tokens a server sends anyway go unused
    client
        .on_login(ok(json!({"version": "1.2.900",
                            "tokens": {"session_token": "s", "refresh_token": "r"}})))
        .unwrap();
    let (_, start) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    assert!(start
        .header(AUTHORIZATION)
        .is_some_and(|a| a.starts_with("Basic ")));
    assert!(client.refresh_request().unwrap().is_none());
    assert!(client.logout_request().is_none());

    let mut client = self::client();
    let info = client.on_login(HttpResponse::new(404, "")).unwrap();
    assert_eq!(info.version, None);
    assert_eq!(info.capability, Capability::default());
}

#[test]
fn a_refused_login() {
    let mut client = client();
    let refused = HttpResponse::new(401, r#"{"error":{"code":2201,"message":"wrong password"}}"#);
    assert!(matches!(client.on_login(refused), Err(Error::Login(e)) if e.code == 2201));
    let refused = ok(json!({"error": {"code": 2201, "message": "denied"}}));
    assert!(matches!(client.on_login(refused), Err(Error::Login(_))));
}

#[test]
fn logout_after_a_login() {
    let client = logged_in();
    let logout = client.logout_request().unwrap();
    assert_eq!(logout.url, format!("{ENDPOINT}/v1/session/logout"));
    assert_eq!(logout.header(AUTHORIZATION), Some("Bearer s1"));
    assert!(self::client().logout_request().is_none());
}

#[test]
fn the_session_travels_and_switches_the_warehouse() {
    let mut client = client();
    let (mut query, _) = client
        .start_query("USE geo", QueryOptions::default())
        .unwrap();
    let mut body = page(json!([]), None);
    body["session"] = json!({"database": "geo", "settings": {"warehouse": "wh2"},
                             "need_sticky": true, "internal": "opaque"});
    query.on_response(&mut client, ok(body)).unwrap();
    assert!(query.is_finished());

    let (_, next) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    assert_eq!(next.header(HEADER_WAREHOUSE), Some("wh2"));
    assert_eq!(next.header(HEADER_STICKY_NODE), Some("node-a"));
    let session = &body_json(&next)["session"];
    assert_eq!(session["database"], "geo");
    assert_eq!(session["internal"], "opaque");
}

#[test]
fn a_transaction_keeps_its_route_hint() {
    let mut client = client();
    let (mut query, begin) = client
        .start_query("BEGIN", QueryOptions::default())
        .unwrap();
    let mut body = page(json!([]), None);
    body["session"] = json!({"txn_state": "Active"});
    let response = ok(body).with_header("x-databend-route-hint", "rh:txn:000000");
    query.on_response(&mut client, response).unwrap();
    let (_, next) = client
        .start_query("INSERT …", QueryOptions::default())
        .unwrap();
    assert_eq!(next.header(HEADER_ROUTE_HINT), Some("rh:txn:000000"));
    assert_ne!(begin.header(HEADER_QUERY_ID), next.header(HEADER_QUERY_ID));
}

#[test]
fn parameters_go_in_the_body() {
    let mut client = client();
    let options = QueryOptions {
        params: Some(json!([1, "a"])),
        ..QueryOptions::default()
    };
    let (_, start) = client.start_query("SELECT ?, ?", options).unwrap();
    assert_eq!(body_json(&start)["params"], json!([1, "a"]));
}

#[test]
fn oversized_bodies_and_ragged_rows_are_refused() {
    let config = ClientConfig::new(ENDPOINT, Auth::basic("root", "")).with_max_body_bytes(64);
    let mut client = Client::new(config, 3).unwrap();
    let (mut query, _) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    let big = ok(page(json!([["1", "a"]]), None));
    assert!(matches!(
        query.on_response(&mut client, big),
        Err(Error::Protocol(_))
    ));

    let mut client = self::client();
    let (mut query, _) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    let ragged = ok(page(json!([["1", "a"], ["2"]]), None));
    assert!(matches!(
        query.on_response(&mut client, ragged),
        Err(Error::Protocol(_))
    ));
}

#[test]
fn a_response_with_no_request_waiting_is_an_error() {
    let mut client = client();
    let (mut query, _) = client
        .start_query("SELECT 1", QueryOptions::default())
        .unwrap();
    query
        .on_response(&mut client, ok(page(json!([]), None)))
        .unwrap();
    let again = query.on_response(&mut client, ok(page(json!([]), None)));
    assert!(matches!(again, Err(Error::Protocol(_))));
}

#[test]
fn hostile_responses_never_panic() {
    let bodies: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"null".to_vec(),
        b"[]".to_vec(),
        b"{".to_vec(),
        vec![0xff, 0xfe, 0x00],
        b"{\"id\":\"q1\"}".to_vec(),
        br#"{"id":"../x","schema":[],"data":[],"state":"Running"}"#.to_vec(),
        br#"{"id":"q1","node_id":"a\r\nb","schema":[],"data":[],"state":"Running"}"#.to_vec(),
        br#"{"id":"q1","schema":[],"data":[[1]],"state":"Running"}"#.to_vec(),
        br#"{"id":"q1","schema":[],"data":[],"state":"x","next_uri":7}"#.to_vec(),
        br#"{"id":"q1","schema":[],"data":[],"state":"x","session":{"settings":{"warehouse":"a\nb"}}}"#
            .to_vec(),
        br#"{"error":{"code":99999999,"message":"x"}}"#.to_vec(),
        "[".repeat(100_000).into_bytes(),
    ];
    for status in [200, 401, 404, 500, 503] {
        for body in &bodies {
            let mut client = client();
            let (mut query, _) = client
                .start_query("SELECT 1", QueryOptions::default())
                .unwrap();
            let response = HttpResponse::new(status, body.clone())
                .with_header("set-cookie", "=")
                .with_header("set-cookie", "a\u{7f}=b")
                .with_header("x-databend-route-hint", "\u{0}");
            let _ = query.on_response(&mut client, response.clone());
            let _ = query.kill_request(&client);
            let _ = query.retry_request(&client);
            let _ = client.on_login(response.clone());
            let _ = client.on_refresh(response);
        }
    }
}
