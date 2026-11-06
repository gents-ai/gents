use super::*;

pub(super) struct SideEffects {
    pub(super) wake_request_id: Option<String>,
    pub(super) created_wake: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_background_tool_completion(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    parent_session_id: &str,
    parent_request_id: &str,
    tool_call_doc_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
    wake: crate::request::lifecycle::RequestHopCause,
) -> Result<()> {
    let parent_request =
        crate::request::binding::load_agent_request_with_access(access, parent_request_id)
            .await?
            .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} not found"))?;

    append_with_parent(
        node,
        access,
        &parent_request,
        parent_session_id,
        parent_request_id,
        tool_call_doc_id,
        tool_name,
        status,
        result,
        reason,
        wake,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_background_tool_completion_for_parent(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    parent_request_doc_id: &str,
    parent_session_id: &str,
    parent_request_id: &str,
    tool_call_doc_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
    wake: crate::request::lifecycle::RequestHopCause,
) -> Result<()> {
    let parent_request = crate::request::binding::load_agent_request_by_doc_id_with_access(
        access,
        parent_request_doc_id,
    )
    .await?
    .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_doc_id} not found"))?;
    append_with_parent(
        node,
        access,
        &parent_request,
        parent_session_id,
        parent_request_id,
        tool_call_doc_id,
        tool_name,
        status,
        result,
        reason,
        wake,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn append_with_parent(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    parent_request: &crate::AgentRequest,
    parent_session_id: &str,
    parent_request_id: &str,
    tool_call_doc_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
    wake: crate::request::lifecycle::RequestHopCause,
) -> Result<()> {
    anyhow::ensure!(
        parent_request.request_id == parent_request_id,
        "background completion parent request mismatch"
    );
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(parent_request.agent_did.as_str()),
        "background completion actor mismatch"
    );
    anyhow::ensure!(
        parent_request.session_id == parent_session_id,
        "background completion parent session mismatch"
    );
    let existing =
        existing_tool_completion_notification(access, parent_request, tool_call_doc_id).await?;
    let tool_call_id = load_tool_call_id(access, tool_call_doc_id).await?;
    let existing_text = match &existing {
        Some(existing) => {
            Some(stored_notification_text(access, parent_request, &existing.doc_id).await?)
        }
        None => None,
    };
    let render = |budget: usize| {
        tool_completion_presentation(&tool_call_id, tool_name, status, result, reason, budget)
    };
    // Replays must preserve the published notice byte-for-byte.
    let published_budget = existing_text
        .as_deref()
        .and_then(|stored| published_notification_budget(stored, &render));
    let output_budget = match published_budget {
        Some(budget) => budget,
        None => crate::tools::surface::configured_output_budget(
            node,
            &parent_request.node_did,
            &parent_request.agent_did,
            tool_name,
        )
        .await
        .min(super::rendering::NOTIFICATION_SUMMARY_BYTES),
    };
    let (notification, presentation) = render(output_budget);
    let key = background_completion_notification_message_key(tool_call_doc_id, "tool");
    let effects = ensure_notification_delivery(
        node,
        access,
        parent_request,
        existing,
        wake,
        NotificationDelivery {
            content: &notification,
            message_key: &key,
            native: crate::request::lifecycle::queue::ToolNotificationPublication {
                tool_call_doc_id: tool_call_doc_id.to_owned(),
                presentation,
            },
        },
    )
    .await?;
    mark_background_tool_notification_delivered(
        access,
        &parent_request.node_did,
        parent_request_id,
        tool_call_doc_id,
    )
    .await?;
    mark_background_tool_completion_side_effects_done(access, tool_call_doc_id).await?;
    tracing::debug!(
        parent_session_id, parent_request_id, tool_call_doc_id,
        wake_request_id = ?effects.wake_request_id,
        created_wake = effects.created_wake,
        "persisted background completion side effects"
    );
    Ok(())
}

async fn stored_notification_text(
    access: &crate::ConfigAccess,
    parent: &crate::AgentRequest,
    notification_doc_id: &str,
) -> Result<String> {
    let (_, message) = crate::session::load_canonical_message(
        access,
        notification_doc_id,
        &parent.node_did,
        parent.requester_did.as_deref(),
    )
    .await
    .with_context(|| format!("reading published background notification {notification_doc_id}"))?;
    message
        .rag_text()
        .context("published background notification has no text")
}

fn published_notification_budget(
    stored: &str,
    render: &impl Fn(usize) -> (String, Vec<gents_protocol::output::PresentationPart>),
) -> Option<usize> {
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

async fn load_tool_call_id(access: &crate::ConfigAccess, tool_call_doc_id: &str) -> Result<String> {
    let doc_id = escape_graphql_string(tool_call_doc_id);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 2) {{ _docID tool_call_id }} }}"#
    );
    let response = access.execute(&query).await?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_value(response["data"]["AgentToolCall"].clone())?;
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

struct NotificationDelivery<'a> {
    content: &'a str,
    message_key: &'a str,
    native: crate::request::lifecycle::queue::ToolNotificationPublication,
}

async fn ensure_notification_delivery(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    parent: &crate::AgentRequest,
    existing: Option<side_effects::ExistingNotification>,
    wake: crate::request::lifecycle::RequestHopCause,
    delivery: NotificationDelivery<'_>,
) -> Result<SideEffects> {
    let enqueued =
        crate::request::lifecycle::queue::persist_background_completion_with_message_canonical_with_access(
            access,
            node,
            parent,
            delivery.content,
            delivery.message_key,
            BACKGROUND_COMPLETION_WAKE_PROMPT,
            RequestQueue {
                delivery: Default::default(),
                position: None,
                source: QueueSource::BackgroundCompletion,
                policy: QueuePolicy::Coalesce,
                key: Some(format!("background_completion:{}", parent.session_id)),
                queued_after_request_id: Some(parent.request_id.clone()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            },
            existing.as_ref().map(|receipt| receipt.doc_id.as_str()),
            &delivery.native,
            wake,
        )
        .await?;
    Ok(SideEffects {
        wake_request_id: enqueued.request.map(|request| request.request_id),
        created_wake: enqueued.created_request,
    })
}

async fn mark_background_tool_notification_delivered(
    access: &crate::ConfigAccess,
    node_did: &str,
    parent_request_id: &str,
    tool_call_doc_id: &str,
) -> Result<()> {
    let node_did = escape_graphql_string(node_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let tool_call_doc_id = escape_graphql_string(tool_call_doc_id);
    let delivered_at = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    node_did: {{ _eq: "{node_did}" }},
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
    access
        .write("mark_background_tool_notification_delivered", &mutation)
        .await?;
    Ok(())
}

async fn mark_background_tool_completion_side_effects_done(
    access: &crate::ConfigAccess,
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
    let response = access.execute(&query).await?;
    let row = response
        .get("data")
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
    let datetime_fields = agent_tool_call_datetime_update_fragment(access, doc_id, &[]).await?;
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
    access
        .write("mark_background_completion_side_effects_done", &mutation)
        .await?;
    Ok(())
}
