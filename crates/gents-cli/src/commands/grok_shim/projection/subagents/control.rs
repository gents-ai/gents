//! Read-only inspection and runtime-owned cancellation of authorized children.

use std::collections::BTreeMap;
use std::sync::Arc;

use gents::descendant_graph::{
    resolve_session_descendant_graph, DescendantEdge, DescendantGraphAccess, DescendantQuery,
};
use gents_protocol::output::TerminalOutput;
use gents_protocol::transcript::present_message;

use super::*;

/// `sessions` comes exclusively from this connection's validated registry.
/// The caller identity matches normal shim submission: agent == requester.
pub(crate) async fn handle(
    node: Arc<EmbeddedNode>,
    principal: &str,
    sessions: &[String],
    method: &str,
    params: &Value,
    context_window: u64,
) -> Result<Value> {
    let edges = authorized_children(&node, principal, sessions).await?;
    if method == SUBAGENT_LIST_RUNNING_METHOD {
        let mut subagents = Vec::new();
        for (_, edge) in edges.values().filter(|(_, edge)| !edge.is_terminal()) {
            if let Some(mut snapshot) = snapshot(&node, edge, context_window).await? {
                if snapshot["status"] == "running" {
                    snapshot
                        .as_object_mut()
                        .expect("snapshot object")
                        .remove("status");
                    subagents.push(snapshot);
                }
            }
        }
        return Ok(json!({"subagents": subagents}));
    }
    let id = params["subagentId"]
        .as_str()
        .context("subagentId required")?;
    let Some((caller, edge)) = edges.get(id) else {
        return Ok(if method == SUBAGENT_GET_METHOD {
            subagent_get_not_found_result()
        } else {
            subagent_cancel_not_found_result(id)
        });
    };
    if method == SUBAGENT_CANCEL_METHOD {
        return match gents::cancel_session_subagent(
            node.clone(),
            caller,
            &edge.child_request_id,
            Some("cancelled from Grok TUI"),
        )
        .await?
        {
            gents::CancelSubagentOutcome::Cancelled(_) => Ok(json!({
                "subagentId": id, "cancelled": true, "outcome": {"kind": "cancelled"},
            })),
            gents::CancelSubagentOutcome::AlreadyTerminal(_) => {
                let snapshot = snapshot(&node, edge, context_window).await?;
                let status = snapshot
                    .as_ref()
                    .and_then(|value| value["status"].as_str())
                    .unwrap_or("completed");
                Ok(json!({"subagentId": id, "cancelled": false,
                    "outcome": {"kind": "already_finished", "status": status}}))
            }
            gents::CancelSubagentOutcome::Unavailable { .. } => {
                Ok(subagent_cancel_not_found_result(id))
            }
            gents::CancelSubagentOutcome::NotAuthorized => {
                anyhow::bail!("subagent is visible but this session cannot control it")
            }
        };
    }
    let block = params
        .get("block")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timeout = params
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(30_000);
    let deadline = tokio::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(timeout))
        .context("subagent timeout exceeds the clock range")?;
    loop {
        let current = snapshot(&node, edge, context_window).await?;
        let terminal = current.as_ref().is_none_or(|value| {
            !matches!(value["status"].as_str(), Some("running" | "initializing"))
        });
        if !block || terminal || tokio::time::Instant::now() >= deadline {
            return Ok(json!({"snapshot": current}));
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + std::time::Duration::from_millis(100)).min(deadline),
        )
        .await;
    }
}

pub(crate) async fn authorized_children(
    node: &EmbeddedNode,
    principal: &str,
    sessions: &[String],
) -> Result<BTreeMap<String, (String, DescendantEdge)>> {
    let mut children = BTreeMap::new();
    for session in sessions {
        // A queued steering/control request can be the newest request in a
        // session without owning the earlier request's child graph. Inspect
        // every physically scoped request admitted for this principal's
        // session; choosing only the latest loses still-live descendants.
        let scope = gents::session::public_request_filter(&gents::session::session_scope_filter(
            principal,
            session,
            Some(principal),
        ));
        let response = gents::graphql::graphql_with_transaction_retry(
            node,
            &format!("{{ AgentRequest(filter: {{ {scope} }}) {{ request_id }} }}"),
            "Grok subagent session callers",
        )
        .await?;
        let callers = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for caller in callers {
            let caller = caller["request_id"]
                .as_str()
                .context("session request omitted logical identity")?
                .to_owned();
            let mut query = DescendantQuery::all(&caller);
            loop {
                let page =
                    resolve_session_descendant_graph(DescendantGraphAccess::Local(node), &query)
                        .await?;
                for edge in page.edges {
                    if !edge.readable() {
                        continue;
                    }
                    let Some(id) = edge
                        .child_session_id
                        .as_ref()
                        .filter(|id| !id.is_empty() && *id != session)
                    else {
                        continue;
                    };
                    // A session ID must resolve uniquely: never choose one of
                    // conflicting child identities by incidental query ordering.
                    if let Some((_, previous)) = children.get(id) {
                        let previous: &DescendantEdge = previous;
                        anyhow::ensure!(
                            previous.child_request_id == edge.child_request_id
                                && previous.child_request_doc_id == edge.child_request_doc_id
                                && previous.principal_did == edge.principal_did
                                && previous.child_requester_did == edge.child_requester_did,
                            "ambiguous subagent session identity"
                        );
                        if edge.controllable() && !previous.controllable() {
                            children.insert(id.clone(), (caller.clone(), edge));
                        }
                    } else {
                        children.insert(id.clone(), (caller.clone(), edge));
                    }
                }
                if !page.has_more {
                    break;
                }
                anyhow::ensure!(
                    page.next_cursor.is_some() && page.next_cursor != query.after,
                    "descendant pagination did not advance"
                );
                query.after = page.next_cursor;
            }
        }
    }
    Ok(children)
}

async fn snapshot(
    node: &Arc<EmbeddedNode>,
    edge: &DescendantEdge,
    context_window: u64,
) -> Result<Option<Value>> {
    let owner = edge
        .principal_did
        .as_deref()
        .context("child principal missing")?;
    let session = edge
        .child_session_id
        .as_deref()
        .context("child session missing")?;
    let physical = edge
        .child_request_doc_id
        .as_deref()
        .context("child physical identity missing")?;
    let owner = escape_graphql_string(owner);
    let requester = edge
        .child_requester_did
        .as_deref()
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    // The descendant owner already selected the immutable physical child.
    // Resolve that document under its principal/requester authority first,
    // then validate its logical/session labels below. Including mutable
    // projection labels in the lookup turns a replicated-label mismatch into
    // a false not-found and hides a broken physical edge.
    let response = gents::graphql::graphql_with_transaction_retry(node, &format!(r#"{{ child: AgentRequest(filter: {{ agent_did: {{_eq: "{owner}"}}, requester_did: {{_eq: {requester}}}, _docID: {{_eq: "{}"}} }}, limit: 2) {{ {CHILD_REQUEST_FIELDS} }} }}"#, escape_graphql_string(physical)), "Grok subagent snapshot").await?;
    let children = decode_rows::<ChildRequestRow>(&response, "child", "child snapshot")?;
    anyhow::ensure!(children.len() <= 1, "duplicate physical child snapshot");
    let child = children
        .first()
        .context("materialized descendant physical child is unavailable")?;
    anyhow::ensure!(
        child.doc_id.as_deref() == Some(physical)
            && child.agent_did == owner
            && child.session_id == session
            && child.requester_did == edge.child_requester_did
            && child.request_id == edge.child_request_id,
        "child snapshot crossed validated physical scope"
    );
    let usage_response = gents::graphql::graphql_with_transaction_retry(
        node,
        &child_usage_query(&children),
        "Grok subagent inference usage",
    )
    .await?;
    let usage = decode_inference_call_rows(&usage_response)?;
    for row in &usage {
        anyhow::ensure!(
            row.request_doc_id == physical
                && row.agent_did == owner
                && row.request_id == child.request_id,
            "child inference usage crossed validated physical scope"
        );
    }
    let usage = usage.iter().collect::<Vec<_>>();
    let tool_response = gents::graphql::graphql_with_transaction_retry(
        node,
        &child_tools_query(&children),
        "Grok subagent tools",
    )
    .await?;
    let tools = decode_child_tool_rows(&tool_response)?;
    for row in &tools {
        anyhow::ensure!(
            row.request_doc_id == physical
                && row.agent_did == owner
                && row.session_id == session
                && row.requester_did == edge.child_requester_did
                && row.request_id == child.request_id,
            "child tool crossed validated physical scope"
        );
    }
    let tools = tools.iter().collect::<Vec<_>>();
    let progress = progress_update(
        child,
        &usage,
        &tools,
        &child.session_id,
        &edge.immediate_parent_session_id,
        context_window,
    );
    let started = child
        .created_at
        .as_deref()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.timestamp_millis().max(0) as u64)
        .unwrap_or(0);
    let status = child
        .finish_status()
        .map(|s| s.wire_name())
        .unwrap_or_else(|| match child.lifecycle_state.as_deref() {
            Some("pending" | "claimed") => "initializing",
            _ => "running",
        });
    let duration = if child.is_terminal() {
        progress.duration_ms
    } else if started == 0 {
        0
    } else {
        (chrono::Utc::now().timestamp_millis().max(0) as u64).saturating_sub(started)
    };
    let mut value = json!({
        "subagentId": child.session_id, "parentSessionId": edge.immediate_parent_session_id,
        "childSessionId": child.session_id, "subagentType": child.behavior_id.as_deref().unwrap_or("general-purpose"),
        "description": spawn_description(None, &std::collections::HashMap::new(), child), "startedAtEpochMs": started, "durationMs": duration, "status": status,
    });
    let object = value.as_object_mut().expect("snapshot object");
    match status {
        "running" => {
            object.extend(json!({"turnCount": progress.turn_count, "toolCallCount": progress.tool_call_count,
                "tokensUsed": progress.tokens_used, "contextWindowTokens": progress.context_window_tokens,
                "contextUsagePct": progress.context_usage_pct, "toolsUsed": progress.tools_used,
                "errorCount": progress.error_count}).as_object().unwrap().clone());
        }
        "completed" => {
            object.insert("output".into(), json!(final_output(node, child).await?));
            object.insert("toolCalls".into(), json!(progress.tool_call_count));
            object.insert("turns".into(), json!(progress.turn_count));
        }
        "failed" => {
            object.insert(
                "failureError".into(),
                json!(child
                    .failure_reason
                    .as_deref()
                    .and_then(nonempty)
                    .unwrap_or("subagent failed")),
            );
        }
        "cancelled" => {
            if let Some(reason) = child.failure_reason.as_deref().and_then(nonempty) {
                object.insert("cancelReason".into(), json!(reason));
            }
        }
        _ => {}
    }
    Ok(Some(value))
}

async fn final_output(node: &Arc<EmbeddedNode>, child: &ChildRequestRow) -> Result<String> {
    let Some(selection) = child.terminal_output.as_ref() else {
        anyhow::bail!("terminal child request omitted canonical terminal output");
    };
    let TerminalOutput::Message { message_doc_id } = selection else {
        return Ok(String::new());
    };
    let request_doc_id = child
        .doc_id
        .as_deref()
        .context("terminal child request omitted physical identity")?;
    let access = gents::ConfigAccess::Local(node.clone());
    let (header, message) = gents::session::load_canonical_message(
        &access,
        message_doc_id,
        &child.agent_did,
        child.requester_did.as_deref(),
    )
    .await
    .context("resolving exact canonical child terminal output")?;
    anyhow::ensure!(
        header.session_id == child.session_id
            && header.request_doc_id.as_deref() == Some(request_doc_id)
            && header.role == gents_protocol::output::MessageRole::Assistant,
        "canonical child terminal output crossed exact request scope"
    );
    Ok(present_message(&message).body_markdown)
}
