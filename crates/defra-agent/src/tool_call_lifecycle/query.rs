//! Read-only queries for tool-call lifecycle reconstruction.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::{FailureClass, ToolCallLifecycle, ToolCallState};

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    message_sequence: u32,
    tool_name: String,
    args: String,
    lifecycle_state: Option<String>,
    started_at: Option<String>,
    tool_failure_class: Option<String>,
}

impl ToolCallLifecycle {
    /// Load an existing AgentToolCall row by session_id and tool_call_id.
    /// Returns `None` if the row does not exist.
    pub async fn load(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<Self>> {
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_tool_call_id = escape_graphql_string(tool_call_id);
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        tool_call_id: {{ _eq: "{escaped_tool_call_id}" }}
                    }},
                    limit: 1
                ) {{
                    _docID
                    message_sequence
                    tool_name
                    args
                    lifecycle_state
                    started_at
                    tool_failure_class
                }}
            }}"#
        );

        let resp = node.execute(&query).await;
        if resp.has_errors() {
            return Err(anyhow!(
                "load AgentToolCall query failed: {:?}",
                resp.errors
            ));
        }

        let rows: Vec<ToolCallRow> = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentToolCall"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let row = match rows.into_iter().next() {
            Some(r) => r,
            None => return Ok(None),
        };

        let state = row
            .lifecycle_state
            .as_deref()
            .and_then(ToolCallState::from_persisted)
            .unwrap_or(ToolCallState::Running); // legacy rows pre-migration default to Running

        let started_at = row
            .started_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let failure_class = row
            .tool_failure_class
            .as_deref()
            .and_then(FailureClass::from_persisted);

        let mut lc = Self::new(
            node,
            session_id.to_string(),
            tool_call_id.to_string(),
            row.message_sequence,
            row.tool_name,
            row.args,
        );
        lc.set_doc_id(Some(row.doc_id));
        lc.set_state(state);
        lc.set_started_at(started_at);
        lc.set_failure_class(failure_class);
        Ok(Some(lc))
    }
}
