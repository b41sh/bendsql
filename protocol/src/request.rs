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

use serde::Serialize;

use crate::config::SensitiveString;
use crate::session::SessionState;

/// The body of `POST /v1/query`.
#[derive(Serialize, Debug)]
pub struct QueryRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionState>,
    sql: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pagination: Option<PaginationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_attachment: Option<StageAttachmentConfig<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct PaginationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_time_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows_in_buffer: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows_per_page: Option<i64>,
}

#[derive(Serialize, Debug, Clone)]
pub struct StageAttachmentConfig<'a> {
    pub location: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_format_options: Option<BTreeMap<&'a str, &'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_options: Option<BTreeMap<&'a str, &'a str>>,
}

impl<'r, 't: 'r> QueryRequest<'r> {
    pub fn new(sql: &'r str) -> QueryRequest<'r> {
        QueryRequest {
            session: None,
            sql,
            pagination: None,
            stage_attachment: None,
            params: None,
        }
    }

    pub fn with_session(mut self, session: Option<SessionState>) -> Self {
        self.session = session;
        self
    }

    pub fn with_pagination(mut self, pagination: Option<PaginationConfig>) -> Self {
        self.pagination = pagination;
        self
    }

    pub fn with_stage_attachment(
        mut self,
        stage_attachment: Option<StageAttachmentConfig<'t>>,
    ) -> Self {
        self.stage_attachment = stage_attachment;
        self
    }

    pub fn with_params(mut self, params: Option<serde_json::Value>) -> Self {
        self.params = params;
        self
    }
}

/// The body of `POST /v1/session/login`.
#[derive(Serialize, Debug)]
pub struct LoginRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<BTreeMap<String, String>>,
}

impl From<&SessionState> for LoginRequest {
    fn from(value: &SessionState) -> Self {
        // The server checks `database` against the default catalog at login, so it is left out
        // when another catalog is selected; each `/v1/query` still applies it with the catalog.
        let database = match value.catalog.as_deref() {
            Some(catalog) if !catalog.eq_ignore_ascii_case("default") => None,
            _ => value.database.clone(),
        };
        Self {
            role: value.role.clone(),
            settings: value.settings.clone(),
            database,
        }
    }
}

/// The body of `POST /v1/session/refresh`.
#[derive(Serialize, Debug)]
pub struct RefreshSessionTokenRequest {
    pub session_token: SensitiveString,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request() {
        let mut session = SessionState::default();
        session.set_database("default");
        let req = QueryRequest::new("select 1")
            .with_session(Some(session))
            .with_pagination(Some(PaginationConfig {
                wait_time_secs: Some(1),
                max_rows_in_buffer: Some(1),
                max_rows_per_page: Some(1),
            }))
            .with_stage_attachment(Some(StageAttachmentConfig {
                location: "@~/my_location",
                file_format_options: None,
                copy_options: None,
            }));
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"session":{"database":"default"},"sql":"select 1","pagination":{"wait_time_secs":1,"max_rows_in_buffer":1,"max_rows_per_page":1},"stage_attachment":{"location":"@~/my_location"}}"#
        );
    }

    #[test]
    fn build_request_with_params() {
        let req = QueryRequest::new("SELECT ? + ?").with_params(Some(serde_json::json!([1, 2])));
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"sql":"SELECT ? + ?","params":[1,2]}"#
        );
        let req = QueryRequest::new("SELECT 1").with_params(None);
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"sql":"SELECT 1"}"#
        );
    }

    #[test]
    fn login_request_keeps_database_for_default_catalog() {
        let mut session = SessionState::default();
        session.set_database("db1");
        assert_eq!(
            LoginRequest::from(&session).database.as_deref(),
            Some("db1")
        );
        session.set_catalog("default");
        assert_eq!(
            LoginRequest::from(&session).database.as_deref(),
            Some("db1")
        );
    }

    #[test]
    fn login_request_skips_database_for_non_default_catalog() {
        let mut session = SessionState::default();
        session.set_catalog("iceberg");
        session.set_database("db1");
        assert_eq!(LoginRequest::from(&session).database, None);
    }

    #[test]
    fn refresh_request_sends_the_token() {
        let req = RefreshSessionTokenRequest {
            session_token: "tok".into(),
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"session_token":"tok"}"#
        );
        assert!(!format!("{req:?}").contains("tok"));
    }
}
