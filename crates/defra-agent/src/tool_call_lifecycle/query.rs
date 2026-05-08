//! Read-only queries for tool-call lifecycle reconstruction.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::{AwaitMode, CancelPolicy, FailureClass, ToolCallLifecycle, ToolCallState};

#[derive(Debug, Clone, Deserialize)]
struct ToolCallResultRow {
    result: String,
}

/// Load the persisted result string for a tool call identified by
/// `session_id` + `tool_call_id`. Returns an error if the row is absent.
pub async fn load_tool_call_result(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<String> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    tool_call_key: {{ _eq: "{tool_call_key}" }}
                }},
                limit: 1
            ) {{
                result
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading tool call result for session_id={} tool_call_id={}: {:?}",
            session_id,
            tool_call_id,
            resp.errors
        );
    }

    let mut rows: Vec<ToolCallResultRow> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    rows.pop().map(|row| row.result).ok_or_else(|| {
        anyhow::anyhow!(
            "loading tool call result: no AgentToolCall for session_id={session_id} tool_call_id={tool_call_id}"
        )
    })
}

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
    // v3 subagent fields — nullable for v2 rows that pre-date the schema migration.
    await_mode: Option<String>,
    cancel_policy: Option<String>,
    child_request_id: Option<String>,
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
                    await_mode
                    cancel_policy
                    child_request_id
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

        // v3 subagent fields. v2 rows (where these columns are null) fall back
        // to the same defaults that Self::new() uses, preserving backwards compat.
        let await_mode = row
            .await_mode
            .as_deref()
            .and_then(AwaitMode::from_persisted)
            .unwrap_or(AwaitMode::Foreground);

        let cancel_policy = row
            .cancel_policy
            .as_deref()
            .and_then(CancelPolicy::from_persisted)
            .unwrap_or(CancelPolicy::Cascade);

        let child_request_id = row.child_request_id.filter(|s| !s.is_empty());

        Ok(Some(Self {
            node,
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            message_sequence: row.message_sequence,
            tool_name: row.tool_name,
            args: row.args,
            doc_id: Some(row.doc_id),
            state,
            started_at,
            failure_class,
            await_mode,
            cancel_policy,
            child_request_id,
        }))
    }
}
