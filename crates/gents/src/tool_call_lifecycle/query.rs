//! Read-only queries for tool-call lifecycle reconstruction.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::{AwaitMode, CancelCause, CancelPolicy, FailureClass, ToolCallLifecycle, ToolCallState};

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
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    requester_did: Option<String>,
    message_sequence: u32,
    tool_name: String,
    args: String,
    lifecycle_state: Option<String>,
    started_at: Option<String>,
    #[serde(default)]
    deadline_at: Option<String>,
    tool_failure_class: Option<String>,
    cancel_cause: Option<String>,
    // v3 subagent fields — nullable for v2 rows that pre-date the schema migration.
    await_mode: Option<String>,
    cancel_policy: Option<String>,
    child_request_id: Option<String>,
    spawn_target_did: Option<String>,
    unclaimed_deadline_at: Option<String>,
    workflow_group_id: Option<String>,
    workflow_role: Option<String>,
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
                    request_id
                    agent_did
                    requester_did
                    message_sequence
                    tool_name
                    args
                    lifecycle_state
                    started_at
                    deadline_at
                    tool_failure_class
                    cancel_cause
                    await_mode
                    cancel_policy
                    child_request_id
                    spawn_target_did
                    unclaimed_deadline_at
                    workflow_group_id
                    workflow_role
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

        let deadline_at = row
            .deadline_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        let failure_class = row
            .tool_failure_class
            .as_deref()
            .and_then(FailureClass::from_persisted);

        let cancel_cause = row
            .cancel_cause
            .as_deref()
            .and_then(CancelCause::from_persisted);

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
        let spawn_target_did = row.spawn_target_did.filter(|s| !s.is_empty());
        let unclaimed_deadline_at = row
            .unclaimed_deadline_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        Ok(Some(Self {
            node,
            request_id: row.request_id.unwrap_or_default(),
            session_id: session_id.to_string(),
            agent_did: row.agent_did.unwrap_or_default(),
            // Current recovery paths only update the existing immutable row,
            // but preserve its route key so a future create transition cannot
            // silently rehydrate the lifecycle as unrouted.
            requester_did: row.requester_did.filter(|value| !value.trim().is_empty()),
            tool_call_id: tool_call_id.to_string(),
            message_sequence: row.message_sequence,
            tool_name: row.tool_name,
            args: row.args,
            doc_id: Some(row.doc_id),
            deadline_at,
            state,
            started_at,
            failure_class,
            cancel_cause,
            await_mode,
            cancel_policy,
            child_request_id,
            spawn_target_did,
            unclaimed_deadline_at,
            workflow_group_id: row.workflow_group_id.filter(|value| !value.is_empty()),
            workflow_role: row.workflow_role.filter(|value| !value.is_empty()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_preserves_immutable_requester_route() {
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_schemas(node.as_ref())
            .await
            .expect("runtime schemas");
        let mut lifecycle = ToolCallLifecycle::new(
            node.clone(),
            "request-routed".to_string(),
            "session-routed".to_string(),
            "did:defra-agent:host".to_string(),
            "tool-call-routed".to_string(),
            1,
            "test_tool".to_string(),
            "{}".to_string(),
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .with_requester_did(Some("did:defra-agent:coordinator".to_string()));
        lifecycle.start_running().await.expect("persist tool call");

        let loaded = ToolCallLifecycle::load(node.clone(), "session-routed", "tool-call-routed")
            .await
            .expect("load tool call")
            .expect("persisted tool call");

        assert_eq!(
            loaded.requester_did.as_deref(),
            Some("did:defra-agent:coordinator")
        );
        node.shutdown().await;
    }
}
