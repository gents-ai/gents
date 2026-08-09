//! Held-tool-call approval watcher.
//!
//! A tool named in `ToolSelection.approval_required_tools` persists its
//! `AgentToolCall` row in `awaitingApproval` (the Lean `holdForApproval`
//! transition) instead of dispatching. This module drives the held call to
//! its verdict: it enumerates the immutable `AgentToolApproval` fact for the
//! physical call (conflicts and twins fail closed) and follows the
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
struct ApprovalCandidateRow {
    #[serde(rename = "_docID")]
    doc_id: String,
}

#[derive(Debug, Deserialize)]
struct ExactHeldApprovalParentRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    agent_did: String,
    lifecycle_state: String,
}

async fn first_approval_decision(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    tool_call_id: &str,
    tool_call_doc_id: &str,
) -> anyhow::Result<Option<crate::tool_call_lifecycle::approval_evidence::VerifiedApprovalDecision>>
{
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentToolApproval(
                filter: {{
                    tool_call_id: {{ _eq: "{escaped_tool_call_id}" }},
                    tool_call_doc_id: {{ _eq: "{}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }}
                }}
            ) {{
                _docID
            }}
        }}"#,
        escape_graphql_string(tool_call_doc_id),
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("AgentToolApproval query failed: {:?}", response.errors);
    }
    let rows: Vec<ApprovalCandidateRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolApproval"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decode AgentToolApproval rows")?
        .unwrap_or_default();

    let [row] = rows.as_slice() else {
        if rows.is_empty() {
            return Ok(None);
        }
        anyhow::bail!("approval logical key has {} physical twins", rows.len());
    };
    let approval = crate::document_version::verified_current_signed_document_version(
        node,
        "AgentToolApproval",
        &row.doc_id,
    )
    .await?;
    let approval = crate::tool_call_lifecycle::approval_evidence::load_verified_exact_approval(
        node, &approval,
    )
    .await?;
    approval.require_identity_binding(tool_call_doc_id, tool_call_id, agent_did)?;
    let pinned_parent = crate::DocumentVersionRef::new(
        &approval.row.tool_call_doc_id,
        &approval.row.tool_call_composite_commit_cid,
    );
    let call = crate::document_version::verified_exact_document_snapshot_with_identity(
        node,
        "AgentToolCall",
        &pinned_parent,
        "tool_call_id agent_did lifecycle_state",
        None,
    )
    .await?;
    let held: ExactHeldApprovalParentRow = call.decode()?;
    if held.doc_id != tool_call_doc_id
        || held.tool_call_id != tool_call_id
        || held.agent_did != agent_did
        || held.lifecycle_state != ToolCallState::AwaitingApproval.as_str()
        || approval.row.tool_call_signer_did != call.source.signer_did
    {
        anyhow::bail!("approval does not pin an exact signed held AgentToolCall version");
    }
    match approval.decision {
        crate::tool_call_lifecycle::approval_evidence::ApprovalDecisionKind::Approved => {
            tracing::info!(
                tool_call_id = %tool_call_id,
                approver_did = %approval.row.approver_did,
                "tool call approved"
            );
        }
        crate::tool_call_lifecycle::approval_evidence::ApprovalDecisionKind::Denied => {
            tracing::info!(
                tool_call_id = %tool_call_id,
                approver_did = %approval.row.approver_did,
                "tool call denied"
            );
        }
    }
    Ok(Some(approval))
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
                map.get(internal_call_id).map(|lifecycle| {
                    (
                        lifecycle.state(),
                        lifecycle.deadline_at(),
                        lifecycle.doc_id().map(str::to_string),
                    )
                })
            };
            let (state, deadline_at, tool_call_doc_id) = match observed {
                Some(entry) => entry,
                None => {
                    return Ok(ToolCallHookAction::skip(format!(
                        "tool call {tool_name} was cancelled or timed out while awaiting approval"
                    )));
                }
            };
            let tool_call_doc_id = tool_call_doc_id.ok_or_else(|| {
                anyhow::anyhow!("held tool call has no persisted physical identity")
            })?;
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

            match first_approval_decision(
                &self.node,
                &self.agent_did,
                internal_call_id,
                &tool_call_doc_id,
            )
            .await?
            {
                Some(approval)
                    if approval.decision
                        == crate::tool_call_lifecycle::approval_evidence::ApprovalDecisionKind::Approved =>
                {
                    let approved = {
                        let mut map = self.in_flight_lifecycles.lock().await;
                        match map.get_mut(internal_call_id) {
                            Some(lifecycle) => lifecycle.approve_and_start(&approval.source).await?,
                            None => continue,
                        }
                    };
                    if approved {
                        return Ok(ToolCallHookAction::Continue);
                    }
                    continue;
                }
                Some(approval) => {
                    let denial = approval.denial_message(tool_name)?;
                    let denied = {
                        let mut map = self.in_flight_lifecycles.lock().await;
                        match map.get_mut(internal_call_id) {
                            Some(lifecycle) => {
                                lifecycle.deny_approval(&denial, &approval.source).await?
                            }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn historical_approval_replays_but_cannot_authorize_an_advanced_held_head() {
        let identity = crate::test_support::signed_test_identity("approval-historical-parent");
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_schemas(&node).await.expect("schemas");

        let deadline = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentToolCall(input: {{
                        tool_call_key: "approval-historical:call-historical",
                        request_id: "req-historical",
                        session_id: "approval-historical",
                        agent_did: "did:test:general",
                        message_sequence: 1,
                        tool_name: "guarded",
                        tool_call_id: "call-historical",
                        args: "{{}}",
                        result: "",
                        status: "called",
                        lifecycle_state: "awaitingApproval",
                        started_at: null,
                        deadline_at: "{deadline}"
                    }}) {{ _docID }}
                }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let access = crate::config_client::ConfigAccess::Local(node.clone());
        let held = crate::config_client::list_held_tool_calls(&access, Some("did:test:general"))
            .await
            .expect("held calls");
        let [held] = held.as_slice() else {
            panic!("expected one held call, got {}", held.len());
        };
        let pinned = crate::document_version::verified_current_signed_document_version(
            &node,
            "AgentToolCall",
            &held.doc_id,
        )
        .await
        .expect("pinned held version");
        crate::config_client::write_tool_approval(
            &access,
            &crate::config_client::ToolApprovalVerdict {
                tool_call_doc_id: held.doc_id.clone(),
                tool_call_id: held.tool_call_id.clone(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-historical".to_string()),
                approve: true,
                approver_did: identity.did().to_string(),
                reason: None,
            },
        )
        .await
        .expect("approval fact");

        // The proposed approve-A/execute-B attack cannot rewrite the invocation
        // payload directly: both fields are schema-immutable.
        let response = node
            .execute(&format!(
                r#"mutation {{ update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{}" }} }},
                    input: {{ args: "rebound", tool_name: "different_tool" }}
                ) {{ _docID }} }}"#,
                escape_graphql_string(&held.doc_id),
            ))
            .await;
        assert!(
            response.has_errors(),
            "immutable args/tool_name rebind unexpectedly succeeded"
        );

        // Even a legal mutable-field update creates a different signed head.
        // Historical approval lookup remains valid for audit/replay, but the
        // transition transaction must refuse to apply it to this newer head.
        let later_deadline = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339();
        let response = node
            .execute(&format!(
                r#"mutation {{ update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{}" }} }},
                    input: {{ deadline_at: "{later_deadline}" }}
                ) {{ _docID }} }}"#,
                escape_graphql_string(&held.doc_id),
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let current = crate::document_version::verified_current_signed_document_version(
            &node,
            "AgentToolCall",
            &held.doc_id,
        )
        .await
        .expect("advanced held version");
        assert_ne!(current.version, pinned.version);

        let approval =
            first_approval_decision(&node, "did:test:general", "call-historical", &held.doc_id)
                .await
                .expect("historical approval parent must verify")
                .expect("approval decision");
        assert_eq!(
            approval.decision,
            crate::tool_call_lifecycle::approval_evidence::ApprovalDecisionKind::Approved
        );

        let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::load(
            node.clone(),
            "approval-historical",
            "call-historical",
        )
        .await
        .expect("load held lifecycle")
        .expect("held lifecycle");
        let error = lifecycle
            .approve_and_start(&approval.source)
            .await
            .expect_err("approval for the earlier held head must be stale");
        assert!(
            format!("{error:#}").contains("changed after its approval evidence was signed"),
            "unexpected stale-parent error: {error:#}"
        );
        assert_eq!(
            lifecycle.state(),
            crate::tool_call_lifecycle::ToolCallState::AwaitingApproval
        );

        let error = lifecycle
            .deny_approval("tool call guarded denied by operator", &approval.source)
            .await
            .expect_err("an approved exact fact cannot be rebound as a denial");
        assert!(
            format!("{error:#}").contains("cannot authorize Denied transition"),
            "unexpected decision-rebind error: {error:#}"
        );

        node.shutdown().await;
    }
}
