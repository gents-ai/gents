//! Held-tool-call approval watcher.
//!
//! A tool named in `ToolSelection.approval_required_tools` persists its
//! `AgentToolCall` row in `awaitingApproval` (the Lean `holdForApproval`
//! transition) instead of dispatching. This module drives the held call to
//! its verdict: it polls for the first matching `AgentToolApproval` document
//! (first decision wins; later documents are ignored) and follows the
//! Lean-fenced edges — `approve` (→ running, tool dispatches), `deny`
//! (→ failed, `approvalDenied`), `cancelWhileHeld` (interrupt), or
//! `timeoutWhileHeld` (the call keeps aging against `deadline_at`; an
//! unanswered approval times out like any other stall).

use std::time::Duration;

use anyhow::Context as _;
use chrono::Utc;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;
use crate::llm::ToolCallHookAction;
use crate::tool_call_lifecycle::ToolCallState;

use super::super::DefraSessionHook;

const APPROVAL_POLL_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Debug, Deserialize)]
struct ApprovalRow {
    decision: Option<String>,
    reason: Option<String>,
    approver_did: Option<String>,
}

#[derive(Debug)]
enum ApprovalDecision {
    Approved,
    Denied { reason: Option<String> },
}

async fn first_approval_decision(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    tool_call_id: &str,
) -> anyhow::Result<Option<ApprovalDecision>> {
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentToolApproval(
                filter: {{
                    tool_call_id: {{ _eq: "{escaped_tool_call_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }}
                }},
                order: {{ created_at: ASC }}
            ) {{
                decision
                reason
                approver_did
            }}
        }}"#
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("AgentToolApproval query failed: {:?}", response.errors);
    }
    let rows: Vec<ApprovalRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolApproval"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decode AgentToolApproval rows")?
        .unwrap_or_default();

    for row in rows {
        match row.decision.as_deref() {
            Some("approved") => {
                tracing::info!(
                    tool_call_id = %tool_call_id,
                    approver_did = row.approver_did.as_deref().unwrap_or(""),
                    "tool call approved"
                );
                return Ok(Some(ApprovalDecision::Approved));
            }
            Some("denied") => {
                tracing::info!(
                    tool_call_id = %tool_call_id,
                    approver_did = row.approver_did.as_deref().unwrap_or(""),
                    "tool call denied"
                );
                return Ok(Some(ApprovalDecision::Denied { reason: row.reason }));
            }
            other => {
                tracing::warn!(
                    tool_call_id = %tool_call_id,
                    decision = other.unwrap_or(""),
                    "ignoring AgentToolApproval with unrecognized decision"
                );
            }
        }
    }
    Ok(None)
}

impl DefraSessionHook {
    pub(crate) async fn drive_held_tool_call(
        &self,
        tool_name: &str,
        internal_call_id: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        loop {
            let observed = {
                let map = self.in_flight_lifecycles.lock().await;
                map.get(internal_call_id)
                    .map(|lifecycle| (lifecycle.state(), lifecycle.deadline_at()))
            };
            let (state, deadline_at) = match observed {
                Some(entry) => entry,
                None => {
                    return Ok(ToolCallHookAction::skip(format!(
                        "tool call {tool_name} was cancelled or timed out while awaiting approval"
                    )));
                }
            };
            match state {
                ToolCallState::AwaitingApproval => {}
                ToolCallState::Cancelled => {
                    self.in_flight_lifecycles
                        .lock()
                        .await
                        .remove(internal_call_id);
                    return Ok(ToolCallHookAction::skip(format!(
                        "tool call {tool_name} was cancelled while awaiting approval"
                    )));
                }
                ToolCallState::TimedOut => {
                    self.in_flight_lifecycles
                        .lock()
                        .await
                        .remove(internal_call_id);
                    return Ok(ToolCallHookAction::skip(format!(
                        "tool call {tool_name} timed out while awaiting approval"
                    )));
                }
                other => {
                    anyhow::bail!(
                        "held tool call {internal_call_id} observed in unexpected state {other:?}"
                    );
                }
            }

            // Held calls keep aging against deadline_at: an unanswered
            // approval must not become a zombie.
            if Utc::now() >= deadline_at {
                let mut map = self.in_flight_lifecycles.lock().await;
                if let Some(lifecycle) = map.get_mut(internal_call_id) {
                    lifecycle.timeout_while_held().await?;
                }
                map.remove(internal_call_id);
                return Ok(ToolCallHookAction::skip(format!(
                    "tool call {tool_name} approval deadline exceeded"
                )));
            }

            match first_approval_decision(&self.node, &self.agent_did, internal_call_id).await? {
                Some(ApprovalDecision::Approved) => {
                    let approved = {
                        let mut map = self.in_flight_lifecycles.lock().await;
                        match map.get_mut(internal_call_id) {
                            Some(lifecycle) => lifecycle.approve_and_start().await?,
                            None => continue,
                        }
                    };
                    if approved {
                        return Ok(ToolCallHookAction::Continue);
                    }
                    continue;
                }
                Some(ApprovalDecision::Denied { reason }) => {
                    let denial = match reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                        Some(reason) => {
                            format!("tool call {tool_name} denied by operator: {reason}")
                        }
                        None => format!("tool call {tool_name} denied by operator"),
                    };
                    let denied = {
                        let mut map = self.in_flight_lifecycles.lock().await;
                        match map.get_mut(internal_call_id) {
                            Some(lifecycle) => lifecycle.deny_approval(&denial).await?,
                            None => continue,
                        }
                    };
                    if denied {
                        self.in_flight_lifecycles
                            .lock()
                            .await
                            .remove(internal_call_id);
                        return Ok(ToolCallHookAction::skip(denial));
                    }
                    continue;
                }
                None => {}
            }

            tokio::time::sleep(APPROVAL_POLL_INTERVAL).await;
        }
    }
}
