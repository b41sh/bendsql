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
use std::collections::{BTreeMap, HashMap};

const TXN_STATE_ACTIVE: &str = "Active";

/// The session the server returns with each response and expects back with the next query.
#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq)]
pub struct SessionState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_roles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub txn_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub need_sticky: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub need_keep_alive: Option<bool>,

    // fields of no interest here that the server needs back with the next query
    #[serde(flatten)]
    additional_fields: HashMap<String, serde_json::Value>,
}

impl SessionState {
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let settings = self.settings.get_or_insert_with(BTreeMap::new);
        settings.insert(key.into(), value.into());
    }

    pub fn set_catalog(&mut self, catalog: impl Into<String>) {
        self.catalog = Some(catalog.into());
    }

    pub fn set_database(&mut self, database: impl Into<String>) {
        self.database = Some(database.into());
    }

    pub fn set_role(&mut self, role: impl Into<String>) {
        self.role = Some(role.into());
    }

    /// Whether a transaction is open: its statements must reach the same node.
    pub fn in_active_transaction(&self) -> bool {
        self.txn_state
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(TXN_STATE_ACTIVE))
    }

    /// Whether the session's state lives on one node (temporary tables, a transaction).
    pub fn need_sticky(&self) -> bool {
        self.need_sticky.unwrap_or(false)
    }

    /// Whether the session holds server state that a logout should release.
    pub fn need_keep_alive(&self) -> bool {
        self.need_keep_alive.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_session_state() {
        let json = r#"{"database":"default","settings":{}}"#;
        let session: SessionState = serde_json::from_str(json).unwrap();
        assert_eq!(session.database.as_deref(), Some("default"));
        assert_eq!(session.settings, Some(BTreeMap::default()));
        assert_eq!(session.role, None);
        assert_eq!(session.secondary_roles, None);

        let json = r#"{"database":"default","settings":{},"role": "role1", "secondary_roles": [], "unknown_field": 1}"#;
        let session: SessionState = serde_json::from_str(json).unwrap();
        assert_eq!(session.role.as_deref(), Some("role1"));
        assert_eq!(session.secondary_roles, Some(vec![]));
    }

    #[test]
    fn unknown_fields_go_back_to_the_server() {
        let json = r#"{"database":"db1","internal":{"a":[1,2]},"last_query_ids":["q1"]}"#;
        let session: SessionState = serde_json::from_str(json).unwrap();
        let back: serde_json::Value = serde_json::to_value(&session).unwrap();
        assert_eq!(back["internal"]["a"][1], 2);
        assert_eq!(back["last_query_ids"][0], "q1");
        assert_eq!(back["database"], "db1");
    }

    #[test]
    fn transaction_and_sticky_flags() {
        let session: SessionState =
            serde_json::from_str(r#"{"txn_state":"active","need_sticky":true}"#).unwrap();
        assert!(session.in_active_transaction());
        assert!(session.need_sticky());
        assert!(!session.need_keep_alive());
        assert!(!SessionState::default().in_active_transaction());
    }
}
