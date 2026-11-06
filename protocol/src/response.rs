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

use serde::{Deserialize, Serialize};

use crate::config::SensitiveString;
use crate::error::ErrorCode;
use crate::session::SessionState;
use crate::settings::QueryResultFormatSettings;

#[derive(Deserialize, Debug, Default, Clone)]
pub struct QueryStats {
    #[serde(flatten)]
    pub progresses: Progresses,
    pub running_time_ms: f64,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Progresses {
    pub scan_progress: ProgressValues,
    pub write_progress: ProgressValues,
    pub result_progress: ProgressValues,
    // optional for older servers
    pub total_scan: Option<ProgressValues>,
    #[serde(default)]
    pub spill_progress: SpillProgress,
}

impl Progresses {
    pub fn has_progress(&self) -> bool {
        self.scan_progress.bytes > 0
            || self.scan_progress.rows > 0
            || self.write_progress.bytes > 0
            || self.write_progress.rows > 0
            || self.result_progress.bytes > 0
            || self.result_progress.rows > 0
            || self
                .total_scan
                .as_ref()
                .is_some_and(|v| v.bytes > 0 || v.rows > 0)
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ProgressValues {
    pub rows: usize,
    pub bytes: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SpillProgress {
    pub file_nums: usize,
    pub bytes: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SchemaField {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

/// A response of `/v1/query` and its pages.
#[derive(Deserialize, Debug)]
pub struct QueryResponse {
    pub id: String,
    pub node_id: Option<String>,
    pub session_id: Option<String>,
    pub session: Option<SessionState>,
    pub schema: Vec<SchemaField>,
    pub data: Vec<Vec<Option<String>>>,
    pub state: String,
    pub settings: Option<QueryResultFormatSettings>,
    pub error: Option<ErrorCode>,
    // optional for older servers
    pub warnings: Option<Vec<String>>,
    // gateway management responses may leave out the engine's progress
    #[serde(default)]
    pub stats: QueryStats,
    pub result_timeout_secs: Option<u64>,
    pub stats_uri: Option<String>,
    pub final_uri: Option<String>,
    pub next_uri: Option<String>,
    pub kill_uri: Option<String>,
}

fn default_session_token_ttl_in_secs() -> u64 {
    3600
}

#[derive(Deserialize, Debug, Clone)]
pub struct SessionTokenInfo {
    pub session_token: SensitiveString,
    #[serde(default = "default_session_token_ttl_in_secs")]
    pub session_token_ttl_in_secs: u64,
    pub refresh_token: SensitiveString,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LoginResponse {
    pub version: String,
    pub tokens: Option<SessionTokenInfo>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum LoginResponseResult {
    Ok(LoginResponse),
    Err { error: ErrorCode },
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum RefreshResponse {
    Ok(SessionTokenInfo),
    Err { error: ErrorCode },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_query_response() {
        let json = r#"{
            "id": "q1", "node_id": "n1", "session_id": "s1",
            "session": {"database": "default"},
            "schema": [{"name": "a", "type": "Int32"}, {"name": "b", "type": "Nullable(String)"}],
            "data": [["1", null]],
            "state": "Succeeded",
            "settings": {"geometry_output_format": "WKB"},
            "error": null, "warnings": ["w"],
            "stats": {"scan_progress": {"rows": 1, "bytes": 2},
                      "write_progress": {"rows": 0, "bytes": 0},
                      "result_progress": {"rows": 1, "bytes": 2},
                      "running_time_ms": 1.5},
            "next_uri": "/v1/query/q1/final", "final_uri": "/v1/query/q1/final",
            "kill_uri": "/v1/query/q1/kill", "stats_uri": "/v1/query/q1",
            "unknown": 1
        }"#;
        let resp: QueryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.schema.len(), 2);
        assert_eq!(resp.data, vec![vec![Some("1".to_string()), None]]);
        assert!(resp.stats.progresses.has_progress());
        assert_eq!(resp.next_uri.as_deref(), Some("/v1/query/q1/final"));
    }

    #[test]
    fn stats_may_be_missing() {
        let json = r#"{"id":"q1","schema":[],"data":[],"state":"Running"}"#;
        let resp: QueryResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.stats.progresses.has_progress());
    }

    #[test]
    fn login_responses() {
        let ok: LoginResponseResult = serde_json::from_str(
            r#"{"version":"v1.2.835","tokens":{"session_token":"s","refresh_token":"r"}}"#,
        )
        .unwrap();
        let LoginResponseResult::Ok(info) = ok else {
            panic!("expected a login")
        };
        let tokens = info.tokens.unwrap();
        assert_eq!(tokens.session_token.inner(), "s");
        assert_eq!(tokens.session_token_ttl_in_secs, 3600);
        assert!(!format!("{tokens:?}").contains("\"s\""));

        let err: LoginResponseResult =
            serde_json::from_str(r#"{"error":{"code":2201,"message":"denied"}}"#).unwrap();
        assert!(matches!(err, LoginResponseResult::Err { error } if error.code == 2201));
    }
}
