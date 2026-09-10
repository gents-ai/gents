//! Read-only inspection and runtime-owned cancellation of authorized children.

use std::collections::BTreeMap;
use std::sync::Arc;

use gents::descendant_graph::{
    resolve_session_descendant_graph, DescendantEdge, DescendantGraphAccess, DescendantQuery,
};
use gents_protocol::message::{AssistantContent, Message};

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
        let head = gents::config_client::ConfigAccess::transact_local(
            node,
            None,
            "grok.subagent_caller",
            |txn| {
                Box::pin(async move {
                    gents::session::load_latest_request_in_txn(
                        txn,
                        principal,
                        session,
                        Some(Some(principal)),
                    )
                    .await
                })
            },
        )
        .await?;
        let Some(head) = head else {
            continue;
        };
        let caller = head.observed.request_id;
        let mut query = DescendantQuery::all(&caller);
        loop {
            let page = resolve_session_descendant_graph(DescendantGraphAccess::Local(node), &query)
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
    Ok(children)
}

async fn snapshot(
    node: &EmbeddedNode,
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
    let scope =
        gents::session::session_scope_filter(owner, session, edge.child_requester_did.as_deref());
    let response = node.execute(&format!(r#"{{ child: AgentRequest(filter: {{ {scope}, _docID: {{_eq: "{}"}}, request_id: {{_eq: "{}"}} }}, limit: 2) {{ {CHILD_REQUEST_FIELDS} }} }}"#, escape_graphql_string(physical), escape_graphql_string(&edge.child_request_id))).await;
    ensure_no_errors(&response, "Grok subagent snapshot")?;
    let children = decode_rows::<ChildRequestRow>(&response, "child", "child snapshot")?;
    anyhow::ensure!(children.len() <= 1, "duplicate physical child snapshot");
    let Some(child) = children.first() else {
        return Ok(None);
    };
    anyhow::ensure!(
        child.doc_id.as_deref() == Some(physical)
            && child.agent_did == owner
            && child.session_id == session
            && child.requester_did == edge.child_requester_did
            && child.request_id == edge.child_request_id,
        "child snapshot crossed validated physical scope"
    );
    let response = node.execute(&child_responses_query(&children)).await;
    ensure_no_errors(&response, "Grok subagent response")?;
    let responses = decode_response_rows(&response)?;
    anyhow::ensure!(
        responses.len() <= 1,
        "duplicate response for physical child"
    );
    for row in &responses {
        anyhow::ensure!(
            row.request_doc_id == physical
                && row.agent_did == owner
                && row.session_id == session
                && row.requester_did == edge.child_requester_did
                && row.request_id == child.request_id,
            "child response crossed validated physical scope"
        );
    }
    let response = responses.first();
    let tool_response = node.execute(&child_tools_query(&children)).await;
    ensure_no_errors(&tool_response, "Grok subagent tools")?;
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
        response,
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
        .finish_status(response)
        .map(|s| s.wire_name())
        .unwrap_or_else(|| match child.lifecycle_state.as_deref() {
            Some("pending" | "claimed") => "initializing",
            _ => "running",
        });
    let duration = if child.is_terminal(response) {
        progress.duration_ms
    } else if started == 0 {
        0
    } else {
        (chrono::Utc::now().timestamp_millis().max(0) as u64).saturating_sub(started)
    };
    let mut value = json!({
        "subagentId": child.session_id, "parentSessionId": edge.immediate_parent_session_id,
        "childSessionId": child.session_id, "subagentType": child.behavior_id.as_deref().unwrap_or("general-purpose"),
        "description": spawn_description(None, child), "startedAtEpochMs": started, "durationMs": duration, "status": status,
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
                    .or(response
                        .and_then(|v| v.error_message.as_deref())
                        .and_then(nonempty))
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

async fn final_output(node: &EmbeddedNode, child: &ChildRequestRow) -> Result<String> {
    let filter = child_result_filter(std::slice::from_ref(child));
    let response = node.execute(&format!(r#"{{ AgentResponse(filter: {{ {filter} }}, limit: 2) {{request_id request_doc_id agent_did requester_did session_id materialized_message_sequence content}} }}"#)).await;
    ensure_no_errors(&response, "Grok child final response")?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(Value::as_array)
        .context("missing child final response rows")?;
    anyhow::ensure!(rows.len() <= 1, "duplicate child final response");
    let row = rows.first();
    if let Some(row) = row {
        anyhow::ensure!(
            row["request_id"].as_str() == Some(child.request_id.as_str())
                && row["request_doc_id"].as_str() == child.doc_id.as_deref()
                && row["agent_did"].as_str() == Some(child.agent_did.as_str())
                && row["session_id"].as_str() == Some(child.session_id.as_str())
                && row.get("requester_did") == Some(&json!(child.requester_did)),
            "child final response crossed physical scope"
        );
    }
    if let Some(sequence) = row.and_then(|v| v["materialized_message_sequence"].as_i64()) {
        let response = node.execute(&format!(r#"{{ AgentMessage(filter: {{ {filter}, sequence: {{_eq: {sequence}}}, role: {{_eq: "assistant"}} }}, limit: 2) {{request_id request_doc_id agent_did requester_did session_id content}} }}"#)).await;
        ensure_no_errors(&response, "Grok child final message")?;
        let messages = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(Value::as_array)
            .context("missing child final message rows")?;
        anyhow::ensure!(messages.len() <= 1, "duplicate child final message");
        if let Some(message) = messages.first() {
            anyhow::ensure!(
                message["request_id"].as_str() == Some(child.request_id.as_str())
                    && message["request_doc_id"].as_str() == child.doc_id.as_deref()
                    && message["agent_did"].as_str() == Some(child.agent_did.as_str())
                    && message["session_id"].as_str() == Some(child.session_id.as_str())
                    && message.get("requester_did") == Some(&json!(child.requester_did)),
                "child final message crossed physical scope"
            );
            let blob = message["content"]
                .as_str()
                .context("child final message content missing")?;
            if let Message::Assistant { content, .. } =
                gents_protocol::transcript::decode_persisted_message("assistant", blob)
            {
                return Ok(content
                    .into_iter()
                    .filter_map(|item| match item {
                        AssistantContent::Text(text) => Some(text.text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""));
            }
        }
    }
    Ok(row
        .and_then(|v| v["content"].as_str())
        .unwrap_or_default()
        .to_owned())
}
