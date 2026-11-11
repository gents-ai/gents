use anyhow::{Context, Result};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow};
use serde::Deserialize;

use gents_protocol::output::rows::TranscriptMessageRow;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::{escape_graphql_string, response_has_documents};

#[derive(Debug, thiserror::Error)]
pub enum ToolAccountingRejection {
    #[error("foreground tool still running at normal request completion")]
    ForegroundRunning,
}

#[derive(Deserialize)]
struct ToolDocument {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_doc_id: String,
    #[serde(rename = "node_did")]
    node_did: String,
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
            || message.node_did != tool.node_did
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
    reader: &mut crate::session::TxnCanonicalReader<'_, '_>,
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
        .node_did
        .as_deref()
        .context("request missing agent")?;
    let session = request
        .session_id
        .as_deref()
        .context("request missing session")?;
    let escaped_request = escape_graphql_string(request_id);
    let response = {
        use gents_protocol::document_query::{
            fields::agent_tool_call as fields, session_scope, Filter, Query, Selection,
        };
        let rendered = Query::select(
            Selection::doc_id()
                .field(fields::TOOL_CALL_KEY)
                .field(fields::REQUEST_DOC_ID)
                .field(fields::NODE_DID)
                .field(fields::REQUESTER_DID)
                .field(fields::SESSION_ID)
                .field(fields::MESSAGE_SEQUENCE)
                .field(fields::TOOL_CALL_ID)
                .field(fields::TOOL_NAME)
                .field(fields::LIFECYCLE_STATE)
                .field(fields::AWAIT_MODE)
                .field(fields::STARTED_AT)
                .field(fields::DEADLINE_AT)
                .field(fields::SPAWNED_BY_TOOL_CALL_DOC_ID)
                .field(fields::PLUGIN_PARENT_TOOL_CALL_DOC_ID)
                .field(fields::PLUGIN_EFFECT_ORDINAL)
                .field(fields::TERMINAL_OUTPUT),
        )
        .filter(session_scope(
            agent,
            session,
            request.requester_did.as_deref(),
        ))
        .filter(Filter::eq(fields::REQUEST_DOC_ID, request_id))
        .render();
        txn.run_local_response(&rendered).await?
    };
    let tools: Vec<ToolDocument> = gents_store::graphql::rows(&response, "AgentToolCall")?;
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
        let plugin_parent = crate::session::plugin_effect_rows::PluginEffectBinding::decode(
            tool.row.plugin_parent_tool_call_doc_id.as_deref(),
            tool.row.plugin_effect_ordinal,
            spawned.is_some(),
            tool.row.tool_call_id.as_deref().unwrap_or_default(),
        )?;
        let mut root = tool;
        let mut seen = rapidhash::RapidHashSet::default();
        let mut live_background_ancestor = false;
        while let Some(parent_id) = root
            .row
            .plugin_parent_tool_call_doc_id
            .as_deref()
            .or(root.row.spawned_by_tool_call_doc_id.as_deref())
        {
            anyhow::ensure!(
                seen.insert(root.doc_id.clone()),
                "cyclic tool invocation ancestry"
            );
            let parent = tools
                .iter()
                .find(|candidate| candidate.doc_id == parent_id)
                .context("derived tool has no parent in its request")?;
            anyhow::ensure!(
                parent.request_doc_id == root.request_doc_id
                    && parent.row.session_id == root.row.session_id
                    && parent.row.message_sequence == root.row.message_sequence,
                "derived tool crosses its parent scope"
            );
            if root.row.spawned_by_tool_call_doc_id.is_some() {
                anyhow::ensure!(
                    parent.row.tool_name.as_deref() == Some("spawn_process")
                        && root.row.await_mode.as_deref() == Some("background"),
                    "invalid spawned invocation"
                );
            }
            live_background_ancestor |= parent.row.await_mode.as_deref() == Some("background")
                && parent.row.lifecycle_state.as_deref() == Some("running");
            root = parent;
        }
        let accepted = directly_bound(headers, root, generation)?;
        if root.doc_id != tool.doc_id && accepted.is_some() {
            crate::tool_call::query::result::load_tool_call_arguments_in_txn(
                txn,
                &tool.doc_id,
                agent,
                session,
                request.requester_did.as_deref(),
            )
            .await?;
            if completed {
                crate::tool_call::query::result::load_tool_call_read_in_txn(
                    txn,
                    &tool.doc_id,
                    agent,
                    session,
                    request.requester_did.as_deref(),
                )
                .await?;
            }
        }
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
        if live_background_ancestor {
            continue;
        }
        if completed && state != "pending" {
            if state == "running" && tool.row.await_mode.as_deref() != Some("background") {
                return Err(ToolAccountingRejection::ForegroundRunning.into());
            }
            if spawned.is_none()
                && plugin_parent.is_none()
                && (state == "running" || tool.row.started_at.is_some())
            {
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
                reader.load_message(&deliveries[0].doc_id).await?;
            }
        }
        let input = if state == "pending" {
            format!(
                r#"lifecycle_state: "cancelled", cancel_cause: "interrupted", completed_at: "{timestamp}""#
            )
        } else if state == "running" && !completed {
            format!(r#"stuck_since: "{timestamp}""#)
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

#[cfg(test)]
mod tests {
    use super::*;
    use gents_store::config_client::ConfigAccess;

}
