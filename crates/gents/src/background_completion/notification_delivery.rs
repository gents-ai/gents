use super::*;

pub(super) struct SideEffects {
    pub(super) notification_sequence: u32,
    pub(super) wake_request_id: Option<String>,
    pub(super) created_notification: bool,
    pub(super) created_wake: bool,
}

pub(crate) async fn append_background_tool_completion(
    node: &EmbeddedNode,
    parent_session_id: &str,
    parent_request_id: &str,
    tool_call_doc_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
) -> Result<()> {
    // Load the parent request up front so the completion notification is stamped
    // with the parent session's owning agent_did.
    let parent_request = crate::request_binding::load_agent_request(node, parent_request_id)
        .await?
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} not found"))?;

    anyhow::ensure!(
        parent_request.session_id == parent_session_id,
        "background completion parent session mismatch"
    );
    let existing =
        existing_tool_completion_notification(node, &parent_request, tool_call_doc_id).await?;
    let tool_call_id = load_tool_call_id(node, tool_call_doc_id).await?;
    let render = |budget: usize| {
        tool_completion_presentation(&tool_call_id, tool_name, status, result, reason, budget)
    };
    // A published notice is replayed exactly (Lean ToolDelivery
    // notification replay), so a redrive renders with the budget that notice
    // was rendered under, not whatever the configuration says now.
    let published_budget = match &existing {
        Some(existing) => {
            published_notification_budget(node, &parent_request, &existing.doc_id, &render).await
        }
        None => None,
    };
    let output_budget = match published_budget {
        Some(budget) => budget,
        None => crate::tool_surface::configured_output_budget(
            node,
            &parent_request.agent_did,
            &parent_request.behavior_id,
            tool_name,
        )
        .await
        .min(super::rendering::NOTIFICATION_SUMMARY_BYTES),
    };
    let (notification, presentation) = render(output_budget);
    let key = background_completion_notification_message_key(tool_call_doc_id, "tool");
    let effects = ensure_notification_delivery(
        node,
        &parent_request,
        existing,
        &notification,
        &key,
        Some(crate::lifecycle::queue::ToolNotificationPublication {
            tool_call_doc_id: tool_call_doc_id.to_owned(),
            presentation,
        }),
    )
    .await?;
    mark_background_tool_notification_delivered(
        node,
        &parent_request.agent_did,
        parent_request_id,
        tool_call_doc_id,
    )
    .await?;
    mark_background_tool_completion_side_effects_done(node, tool_call_doc_id).await?;
    tracing::debug!(
        parent_session_id, parent_request_id, tool_call_doc_id,
        wake_request_id = ?effects.wake_request_id,
        created_wake = effects.created_wake,
        "persisted background completion side effects"
    );
    Ok(())
}

/// The summary budget an already published notice was rendered under: the
/// unescaped length of its `<result>` body, with or without a truncation
/// marker, whichever re-renders the stored text exactly. `None` when the
/// stored notice cannot be read or matches neither, which leaves the atomic
/// publication owner to reject the conflicting replay.
async fn published_notification_budget(
    node: &EmbeddedNode,
    parent: &crate::AgentRequest,
    notification_doc_id: &str,
    render: &impl Fn(usize) -> (String, Vec<gents_protocol::output::PresentationPart>),
) -> Option<usize> {
    let (_, message) = crate::session::load_canonical_message_from_node(
        node,
        notification_doc_id,
        &parent.agent_did,
        parent.requester_did.as_deref(),
    )
    .await
    .map_err(|error| {
        tracing::warn!(
            notification_doc_id,
            error = %format!("{error:#}"),
            "published background notification could not be read for replay"
        )
    })
    .ok()?;
    let stored = message.rag_text()?;
    let body = stored.split_once("<result>")?.1.split_once("</result>")?.0;
    let unescaped = body
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let candidates = [
        unescaped.strip_suffix("...").map(str::len),
        Some(unescaped.len()),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter(|budget| *budget <= super::rendering::NOTIFICATION_SUMMARY_BYTES)
        .find(|budget| render(*budget).0 == stored)
}

async fn load_tool_call_id(node: &EmbeddedNode, tool_call_doc_id: &str) -> Result<String> {
    let doc_id = escape_graphql_string(tool_call_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 2) {{ _docID tool_call_id }} }}"#
        ))
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "query background tool notification identity failed: {:?}",
        response.errors
    );
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "AgentToolCall")?;
    anyhow::ensure!(
        rows.len() == 1,
        "background tool notification requires one exact physical lifecycle row"
    );
    rows[0]
        .get("tool_call_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .context("background tool notification lifecycle omitted tool_call_id")
}

/// Marker discovery supplies only an ID; the atomic owner reloads receipt and
/// Goal together before deciding whether any wake can be published.
pub(super) async fn ensure_notification_delivery(
    node: &EmbeddedNode,
    parent: &crate::AgentRequest,
    existing: Option<side_effects::ExistingNotification>,
    content: &str,
    message_key: &str,
    native: Option<crate::lifecycle::queue::ToolNotificationPublication>,
) -> Result<SideEffects> {
    let native = native.context(
        "subagent background notification requires its dedicated canonical provenance builder",
    )?;
    let enqueued = crate::lifecycle::queue::persist_background_completion_with_message_canonical(
        node,
        parent,
        content,
        message_key,
        BACKGROUND_COMPLETION_WAKE_PROMPT,
        RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{}", parent.session_id)),
            queued_after_request_id: Some(parent.request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        existing.as_ref().map(|receipt| receipt.doc_id.as_str()),
        &native,
    )
    .await?;
    Ok(SideEffects {
        notification_sequence: enqueued.message_sequence,
        wake_request_id: enqueued.request.map(|request| request.request_id),
        created_notification: existing.is_none(),
        created_wake: enqueued.created_request,
    })
}

async fn mark_background_tool_notification_delivered(
    node: &EmbeddedNode,
    agent_did: &str,
    parent_request_id: &str,
    tool_call_doc_id: &str,
) -> Result<()> {
    let agent_did = escape_graphql_string(agent_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let tool_call_doc_id = escape_graphql_string(tool_call_doc_id);
    let delivered_at = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    request_id: {{ _eq: "{parent_request_id}" }},
                    _docID: {{ _eq: "{tool_call_doc_id}" }},
                    completion_notification_delivered_at: {{ _eq: null }}
                }},
                input: {{
                    completion_notification_delivered_at: "{delivered_at}"
                }}
            ) {{ _docID }}
        }}"#
    );
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "mark_background_tool_notification_delivered",
        &mutation,
    )
    .await?;
    Ok(())
}

async fn mark_background_tool_completion_side_effects_done(
    node: &EmbeddedNode,
    tool_call_doc_id: &str,
) -> Result<()> {
    let tool_call_doc_id = escape_graphql_string(tool_call_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ _docID: {{ _eq: "{tool_call_doc_id}" }} }},
                limit: 1
            ) {{ _docID status lifecycle_state }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query background completion tool row failed: {:?}",
            response.errors
        );
    }
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow!("background completion tool row {tool_call_doc_id} not found"))?;
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("background completion tool row {tool_call_doc_id} not found"))?;
    let status = row
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if status == "completed" {
        return Ok(());
    }
    let lifecycle_state = row
        .get("lifecycle_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !(status == "completionPending" || status.starts_with("completionPending:"))
        || !matches!(
            lifecycle_state,
            "completed" | "failed" | "timedOut" | "cancelled"
        )
    {
        anyhow::bail!(
            "background completion tool row {tool_call_doc_id} is not awaiting terminal side effects"
        );
    }
    let escaped_doc_id = escape_graphql_string(doc_id);
    let escaped_status = escape_graphql_string(status);
    let datetime_fields = agent_tool_call_datetime_update_fragment(node, doc_id, &[]).await?;
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    status: {{ _eq: "{escaped_status}" }}
                }},
                input: {{ status: "completed"{datetime_fields} }}
            ) {{ _docID }}
        }}"#
    );
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "mark_background_completion_side_effects_done",
        &mutation,
    )
    .await?;
    Ok(())
}
