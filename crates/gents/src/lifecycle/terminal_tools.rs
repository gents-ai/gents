//! Exact accepted-header tool accounting inside the request terminal transaction.
//! Handoff records uncertainty; a running executor is never declared stopped.
use anyhow::{Context, Result};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow};
use serde::Deserialize;

use crate::config_client::ConfigApplyTxn;
use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session::canonical_rows::TranscriptMessageRow;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolAccountingRejection {
    #[error("foreground tool still running at normal request completion")]
    ForegroundRunning,
}

#[derive(Deserialize)]
struct ToolDocument {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_doc_id: String,
    agent_did: String,
    #[serde(flatten)]
    row: AgentToolCallRow,
}

fn directly_bound<'a>(
    headers: &'a [TranscriptMessageRow],
    tool: &ToolDocument,
    generation: &str,
) -> Result<Option<(&'a TranscriptMessageRow, &'a str)>> {
    let mut found = None;
    for header in headers {
        let message = &header.message;
        if message.request_doc_id.as_deref() != Some(tool.request_doc_id.as_str())
            || message.session_id.as_str() != tool.row.session_id.as_deref().unwrap_or_default()
            || Some(i64::from(message.sequence)) != tool.row.message_sequence
            || message.agent_did != tool.agent_did
            || message.requester_did != tool.row.requester_did
            || message.role != MessageRole::Assistant
            || message.outcome != OutputOutcome::Complete
            || !matches!(&message.publication, MessagePublication::RequestExecution { execution_generation } if execution_generation == generation)
        {
            continue;
        }
        for block in &message.blocks {
            if let MessageBlock::ToolCall {
                tool_call_doc_id,
                id,
                name,
                ..
            } = block
            {
                if tool_call_doc_id == &tool.doc_id {
                    anyhow::ensure!(
                        found.is_none(),
                        "tool has ambiguous accepted header membership"
                    );
                    anyhow::ensure!(
                        tool.row.tool_call_id.as_deref() == Some(id.as_str())
                            && tool.row.tool_name.as_deref() == Some(name.as_str()),
                        "physical tool disagrees with accepted native intent"
                    );
                    found = Some((header, name.as_str()));
                }
            }
        }
    }
    Ok(found)
}

pub(super) async fn account_tools_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request: &AgentRequestRow,
    headers: &[TranscriptMessageRow],
    generation: &str,
    completed: bool,
    timestamp: &str,
) -> Result<()> {
    let request_id = request
        .doc_id
        .as_deref()
        .context("request has no physical identity")?;
    let agent = request
        .agent_did
        .as_deref()
        .context("request missing agent")?;
    let session = request
        .session_id
        .as_deref()
        .context("request missing session")?;
    let scope =
        crate::session::session_scope_filter(agent, session, request.requester_did.as_deref());
    let escaped_request = escape_graphql_string(request_id);
    let response = txn
        .execute_local_response(&format!(
            r#"{{
        AgentToolCall(filter: {{ {scope}, request_doc_id: {{ _eq: "{escaped_request}" }} }}) {{
            _docID tool_call_key request_doc_id agent_did requester_did session_id
            message_sequence tool_call_id tool_name lifecycle_state await_mode cancel_policy
            started_at child_request_id spawned_by_tool_call_doc_id delegated_input
        }}
    }}"#
        ))
        .await?;
    let tools: Vec<ToolDocument> = crate::graphql::rows(&response, "AgentToolCall")?;
    // Every accepted intent must resolve its exact physical lifecycle document.
    for header in headers.iter().filter(|h| matches!(&h.message.publication,
        MessagePublication::RequestExecution { execution_generation } if execution_generation == generation)
        && h.message.outcome == OutputOutcome::Complete && h.message.role == MessageRole::Assistant) {
        for block in &header.message.blocks {
            if let MessageBlock::ToolCall { tool_call_doc_id, .. } = block {
                anyhow::ensure!(tools.iter().filter(|tool| &tool.doc_id == tool_call_doc_id).count() == 1,
                    "accepted tool lifecycle is missing or ambiguous");
            }
        }
    }
    let timestamp = escape_graphql_string(timestamp);
    for tool in &tools {
        let spawned = tool.row.spawned_by_tool_call_doc_id.as_deref();
        let accepted = if let Some(parent_id) = spawned {
            let Some(parent) = tools.iter().find(|candidate| candidate.doc_id == parent_id) else {
                continue;
            };
            if parent.row.spawned_by_tool_call_doc_id.is_some()
                || parent.doc_id == tool.doc_id
                || parent.request_doc_id != tool.request_doc_id
                || parent.row.session_id != tool.row.session_id
                || parent.row.message_sequence != tool.row.message_sequence
            {
                continue;
            }
            let binding = directly_bound(headers, parent, generation)?;
            if binding.is_some() {
                anyhow::ensure!(
                    binding.unwrap().1 == "spawn_process"
                        && tool.row.await_mode.as_deref() == Some("background")
                        && tool.row.child_request_id.is_none(),
                    "invalid spawned execution provenance"
                );
            }
            binding
        } else {
            directly_bound(headers, tool, generation)?
        };
        let Some((accepted_header, _accepted_name)) = accepted else {
            continue;
        };
        let state = tool
            .row
            .lifecycle_state
            .as_deref()
            .context("owned tool missing lifecycle")?;
        anyhow::ensure!(
            matches!(
                state,
                "pending" | "running" | "completed" | "failed" | "timedOut" | "cancelled"
            ),
            "owned tool has invalid lifecycle"
        );
        if completed && state != "pending" {
            if state == "running" {
                if tool.row.await_mode.as_deref() != Some("background") {
                    return Err(ToolAccountingRejection::ForegroundRunning.into());
                }
            }
            if spawned.is_none() && (state == "running" || tool.row.started_at.is_some()) {
                let intent = accepted_header
                    .message
                    .blocks
                    .iter()
                    .find_map(|block| match block {
                        MessageBlock::ToolCall {
                            tool_call_doc_id,
                            id,
                            call_id,
                            ..
                        } if tool_call_doc_id == &tool.doc_id => Some((id, call_id)),
                        _ => None,
                    })
                    .context("accepted tool intent disappeared")?;
                let deliveries = headers
                    .iter()
                    .filter(|header| {
                        super::terminal_binding::is_exact_invocation_reply(
                            &header.message,
                            request_id,
                            session,
                            &tool.doc_id,
                            intent.0,
                            intent.1,
                        )
                    })
                    .collect::<Vec<_>>();
                if deliveries.len() != 1 {
                    let candidates = headers
                        .iter()
                        .filter_map(|header| match &header.message.publication {
                            MessagePublication::ToolDelivery { tool_call_doc_id }
                                if tool_call_doc_id == &tool.doc_id =>
                            {
                                Some(format!("{}:{:?}", header.doc_id, header.message.blocks))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    anyhow::bail!(
                        "started invocation has no unique durable reply: tool_doc_id={}, accepted_id={}, accepted_call_id={:?}, exact_replies={}, physical_delivery_candidates={:?}",
                        tool.doc_id,
                        intent.0,
                        intent.1,
                        deliveries.len(),
                        candidates
                    );
                }
                crate::session::load_canonical_message_in_txn(
                    txn,
                    &deliveries[0].doc_id,
                    agent,
                    request.requester_did.as_deref(),
                )
                .await?;
            }
        }
        let input = if state == "pending" {
            // No executor ever started: cancel immutable intent, not an effect.
            format!(
                r#"lifecycle_state: "cancelled", cancel_cause: "interrupted", completed_at: "{timestamp}""#
            )
        } else if state == "running" && !completed {
            let cascade = tool
                .row
                .cancel_policy
                .as_deref()
                .context("running tool missing cancel policy")?;
            anyhow::ensure!(
                matches!(cascade, "cascade" | "detach"),
                "invalid tool cancellation policy"
            );
            let mut fields = format!(r#"stuck_since: "{timestamp}""#);
            if cascade == "cascade" {
                fields.push_str(&format!(r#", cancel_cascade_intent_at: "{timestamp}""#));
                if tool.row.delegated_input.is_some() {
                    fields.push_str(", cancel_pending_remote_ack: true");
                }
            }
            fields
        } else {
            continue;
        };
        let tool_id = escape_graphql_string(&tool.doc_id);
        let state = escape_graphql_string(state);
        let response = txn.execute_local_response(&format!(r#"mutation {{
            update_AgentToolCall(filter: {{ _docID: {{ _eq: "{tool_id}" }},
                request_doc_id: {{ _eq: "{escaped_request}" }}, lifecycle_state: {{ _eq: "{state}" }} }},
                input: {{ {input} }}) {{ _docID }}
        }}"#)).await?;
        anyhow::ensure!(
            response
                .data
                .as_ref()
                .and_then(|data| data.get("update_AgentToolCall"))
                .is_some_and(response_has_documents),
            "owned tool accounting lost lifecycle CAS"
        );
    }
    Ok(())
}
