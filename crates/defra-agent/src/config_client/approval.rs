//! Tool-call approval client: list held calls and write verdict documents.
//!
//! Shared by the CLI (`tools holds` / `tools approve`) and the desktop
//! bridge. An operator approves by writing an `AgentToolApproval` document —
//! same shape as every other control-plane action; the runtime's verdict
//! watcher (hook/persistence/approval.rs) notices and drives the Lean-fenced
//! approve/deny edge. First decision per call wins.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::ConfigAccess;

/// A tool call persisted in `awaitingApproval`, as surfaced to operators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldToolCall {
    pub tool_call_id: String,
    pub request_id: Option<String>,
    pub session_id: Option<String>,
    pub agent_did: Option<String>,
    pub tool_name: Option<String>,
    pub args: Option<String>,
    pub deadline_at: Option<String>,
}

/// List every tool call currently held for approval, optionally scoped to one
/// agent DID.
pub async fn list_held_tool_calls(
    access: &ConfigAccess,
    agent_did: Option<&str>,
) -> Result<Vec<HeldToolCall>> {
    let agent_filter = agent_did
        .map(|did| {
            let escaped = escape_graphql_string(did);
            format!(r#", agent_did: {{ _eq: "{escaped}" }}"#)
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ lifecycle_state: {{ _eq: "awaitingApproval" }}{agent_filter} }},
                order: {{ deadline_at: ASC }}
            ) {{
                tool_call_id
                request_id
                session_id
                agent_did
                tool_name
                args
                deadline_at
            }}
        }}"#
    );
    let response = access.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentToolCall"))
        .cloned()
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    serde_json::from_value(rows).context("decode held AgentToolCall rows")
}

/// Verdict to record for a held tool call.
#[derive(Debug, Clone)]
pub struct ToolApprovalVerdict {
    pub tool_call_id: String,
    pub agent_did: String,
    pub request_id: Option<String>,
    /// true = approved, false = denied.
    pub approve: bool,
    pub approver_did: String,
    pub reason: Option<String>,
}

/// Write the `AgentToolApproval` decision document. Returns the approval_id.
pub async fn write_tool_approval(
    access: &ConfigAccess,
    verdict: &ToolApprovalVerdict,
) -> Result<String> {
    let approval_id = format!("approval-{}-{}", verdict.tool_call_id, uuid::Uuid::new_v4());
    let escaped_approval_id = escape_graphql_string(&approval_id);
    let escaped_tool_call_id = escape_graphql_string(&verdict.tool_call_id);
    let escaped_agent_did = escape_graphql_string(&verdict.agent_did);
    let escaped_approver_did = escape_graphql_string(&verdict.approver_did);
    let decision = if verdict.approve {
        "approved"
    } else {
        "denied"
    };
    let request_id_field = verdict
        .request_id
        .as_deref()
        .map(|request_id| {
            let escaped = escape_graphql_string(request_id);
            format!(r#"request_id: "{escaped}","#)
        })
        .unwrap_or_default();
    let reason_field = verdict
        .reason
        .as_deref()
        .map(|reason| {
            let escaped = escape_graphql_string(reason);
            format!(r#"reason: "{escaped}","#)
        })
        .unwrap_or_default();
    let created_at = chrono::Utc::now().to_rfc3339();

    let mutation = format!(
        r#"mutation {{
            create_AgentToolApproval(input: {{
                approval_id: "{escaped_approval_id}",
                tool_call_id: "{escaped_tool_call_id}",
                {request_id_field}
                agent_did: "{escaped_agent_did}",
                decision: "{decision}",
                approver_did: "{escaped_approver_did}",
                {reason_field}
                created_at: "{created_at}"
            }}) {{ _docID }}
        }}"#
    );
    access
        .execute(&mutation)
        .await
        .context("create AgentToolApproval")?;
    Ok(approval_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_and_list_round_trip_against_local_node() {
        let data_path =
            std::env::temp_dir().join(format!("agent-approval-client-{}", uuid::Uuid::new_v4()));
        let node = defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap();
        crate::ensure_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.into());

        // Persist a held row shaped like the runtime's hold_for_approval.
        let deadline = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
        access
            .execute(&format!(
                r#"mutation {{
                    create_AgentToolCall(input: {{
                        tool_call_key: "session-client:call-client",
                        request_id: "req-client",
                        session_id: "session-client",
                        agent_did: "did:defra-agent:general",
                        message_sequence: 1,
                        tool_name: "guarded",
                        tool_call_id: "call-client",
                        args: "{{}}",
                        result: "",
                        status: "called",
                        lifecycle_state: "awaitingApproval",
                        started_at: null,
                        deadline_at: "{deadline}"
                    }}) {{ _docID }}
                }}"#
            ))
            .await
            .unwrap();

        let held = list_held_tool_calls(&access, Some("did:defra-agent:general"))
            .await
            .unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].tool_call_id, "call-client");
        assert_eq!(held[0].tool_name.as_deref(), Some("guarded"));

        let approval_id = write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_id: "call-client".to_string(),
                agent_did: "did:defra-agent:general".to_string(),
                request_id: Some("req-client".to_string()),
                approve: false,
                approver_did: "did:key:operator".to_string(),
                reason: Some("blocked in test".to_string()),
            },
        )
        .await
        .unwrap();
        assert!(approval_id.starts_with("approval-call-client-"));

        let decision = access
            .execute(
                r#"{ AgentToolApproval(filter: { tool_call_id: { _eq: "call-client" } }) { decision reason approver_did } }"#,
            )
            .await
            .unwrap();
        let rows = decision
            .get("data")
            .and_then(|data| data.get("AgentToolApproval"))
            .and_then(|rows| rows.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("decision").and_then(|value| value.as_str()),
            Some("denied")
        );

        let _ = std::fs::remove_dir_all(&data_path);
    }
}
