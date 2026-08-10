use super::*;

pub(super) struct SideEffects {
    pub(super) notification_sequence: u32,
    pub(super) wake_request_id: String,
    pub(super) created_notification: bool,
    pub(super) created_wake: bool,
}

pub(crate) async fn append_background_tool_completion(
    node: &EmbeddedNode,
    parent_session_id: &str,
    parent_request_id: &str,
    tool_call_id: &str,
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

    let queue_key = format!("background_completion:{parent_session_id}");
    let (notification_timestamp, created_notification) =
        match existing_tool_completion_notification(node, parent_session_id, tool_call_id).await? {
            Some(existing) => {
                if bound_background_wake_request(node, &existing, &queue_key)
                    .await?
                    .is_some()
                {
                    mark_background_tool_notification_delivered(
                        node,
                        &parent_request.agent_did,
                        parent_request_id,
                        tool_call_id,
                    )
                    .await?;
                    mark_background_tool_completion_side_effects_done(
                        node,
                        parent_session_id,
                        tool_call_id,
                    )
                    .await?;
                    return Ok(());
                }
                (existing.timestamp, false)
            }
            None => {
                let notification =
                    render_tool_completion(tool_call_id, tool_name, status, result, reason);
                let notification_message_key =
                    background_completion_notification_message_key(tool_call_id, "tool");
                let enqueued = enqueue_background_completion_with_message(
                    node,
                    &parent_request,
                    &notification,
                    &notification_message_key,
                    BACKGROUND_COMPLETION_WAKE_PROMPT,
                    QueueHints {
                        source: QueueSource::BackgroundCompletion,
                        policy: QueuePolicy::Coalesce,
                        key: Some(queue_key.clone()),
                        queued_after_request_id: Some(parent_request_id.to_string()),
                        interrupted_request_id: None,
                    },
                )
                .await?;
                mark_background_tool_notification_delivered(
                    node,
                    &parent_request.agent_did,
                    parent_request_id,
                    tool_call_id,
                )
                .await?;
                mark_background_tool_completion_side_effects_done(
                    node,
                    parent_session_id,
                    tool_call_id,
                )
                .await?;
                tracing::debug!(
                    parent_session_id,
                    parent_request_id,
                    tool_call_id,
                    wake_request_id = %enqueued.request.request_id,
                    created_wake = enqueued.created_request,
                    "persisted background tool notification and consuming wake atomically"
                );
                return Ok(());
            }
        };

    if existing_wakeup_after(node, parent_session_id, &queue_key, &notification_timestamp)
        .await?
        .is_some()
    {
        mark_background_tool_notification_delivered(
            node,
            &parent_request.agent_did,
            parent_request_id,
            tool_call_id,
        )
        .await?;
        mark_background_tool_completion_side_effects_done(node, parent_session_id, tool_call_id)
            .await?;
        return Ok(());
    }

    let _wake = enqueue_session_request(
        node,
        &parent_request,
        BACKGROUND_COMPLETION_WAKE_PROMPT,
        ExecutionOrigin::Scheduled,
        QueueHints {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(queue_key),
            queued_after_request_id: Some(parent_request_id.to_string()),
            interrupted_request_id: None,
        },
    )
    .await?;
    mark_background_tool_notification_delivered(
        node,
        &parent_request.agent_did,
        parent_request_id,
        tool_call_id,
    )
    .await?;
    mark_background_tool_completion_side_effects_done(node, parent_session_id, tool_call_id)
        .await?;

    if created_notification {
        tracing::debug!(
            parent_session_id,
            parent_request_id,
            tool_call_id,
            "appended background tool completion notification"
        );
    }
    Ok(())
}

async fn mark_background_tool_notification_delivered(
    node: &EmbeddedNode,
    agent_did: &str,
    parent_request_id: &str,
    tool_call_id: &str,
) -> Result<()> {
    let agent_did = escape_graphql_string(agent_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let delivered_at = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    request_id: {{ _eq: "{parent_request_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }},
                    completion_notification_delivered_at: {{ _eq: null }}
                }},
                input: {{
                    completion_notification_delivered_at: "{delivered_at}"
                }}
            ) {{ _docID }}
        }}"#
    );
    crate::session::execute_mutation_with_retry(
        node,
        &mutation,
        "mark_background_tool_notification_delivered",
    )
    .await?;
    Ok(())
}

async fn mark_background_tool_completion_side_effects_done(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<()> {
    let tool_call_key = escape_graphql_string(&format!("{session_id}:{tool_call_id}"));
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ tool_call_key: {{ _eq: "{tool_call_key}" }} }},
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
        .ok_or_else(|| anyhow!("background completion tool row {tool_call_key} not found"))?;
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("background completion tool row {tool_call_key} not found"))?;
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
            "background completion tool row {tool_call_key} is not awaiting terminal side effects"
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
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "mark background completion side effects done failed: {:?}",
            response.errors
        );
    }
    Ok(())
}
