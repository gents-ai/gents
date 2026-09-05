use super::*;
use anyhow::Context as _;

pub(super) async fn ensure_projection_side_effects(
    node: &EmbeddedNode,
    parent_session_id: &str,
    parent_request_id: &str,
    edge: &ChildEdge,
    status: &str,
    summary: &str,
) -> Result<SideEffects> {
    // Load the parent request up front so the projection notification is stamped
    // with the parent session's owning agent_did.
    let parent_request = crate::request_binding::load_agent_request(node, parent_request_id)
        .await?
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} not found"))?;

    let queue_key = format!("background_completion:{parent_session_id}");
    let (notification_sequence, notification_timestamp, created_notification) =
        match existing_notification(node, parent_session_id, &edge.child_request_id).await? {
            Some(existing) => {
                if let Some(wake_request_id) =
                    bound_background_wake_request(node, &existing, &queue_key).await?
                {
                    return Ok(SideEffects {
                        notification_sequence: existing.sequence,
                        wake_request_id,
                        created_notification: false,
                        created_wake: false,
                    });
                }
                (existing.sequence, existing.timestamp, false)
            }
            None => {
                let notification = render_notification(edge, status, summary);
                let notification_message_key = background_completion_notification_message_key(
                    &edge.child_request_id,
                    "subagent",
                );
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
                return Ok(SideEffects {
                    notification_sequence: enqueued.message_sequence,
                    wake_request_id: enqueued.request.request_id,
                    created_notification: true,
                    created_wake: enqueued.created_request,
                });
            }
        };

    if let Some(wake_request_id) =
        existing_wakeup_after(node, parent_session_id, &queue_key, &notification_timestamp).await?
    {
        return Ok(SideEffects {
            notification_sequence,
            wake_request_id,
            created_notification,
            created_wake: false,
        });
    }

    let wake = enqueue_session_request(
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

    Ok(SideEffects {
        notification_sequence,
        wake_request_id: wake.request_id,
        created_notification,
        created_wake: true,
    })
}

pub(super) fn bridge_state_is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "timedOut" | "cancelled")
}

pub(super) struct ExistingNotification {
    pub(super) sequence: u32,
    pub(super) timestamp: DateTime<Utc>,
    pub(super) request_id: Option<String>,
    pub(super) request_doc_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NotificationMessageRow {
    sequence: u32,
    content: String,
    timestamp: String,
    request_id: Option<String>,
    request_doc_id: Option<String>,
}

async fn existing_notification(
    node: &EmbeddedNode,
    parent_session_id: &str,
    child_request_id: &str,
) -> Result<Option<ExistingNotification>> {
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                content
                timestamp
                request_id
                request_doc_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent AgentMessage notifications for session {parent_session_id} failed: {:?}",
            response.errors
        );
    }

    let marker = format!(
        r#"child_request_id="{}""#,
        xml_escape_attr(child_request_id)
    );
    let rows: Vec<NotificationMessageRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    for row in rows {
        if row.content.contains("<subagent-notification") && row.content.contains(&marker) {
            return Ok(Some(ExistingNotification {
                sequence: row.sequence,
                timestamp: parse_utc_timestamp(&row.timestamp, "AgentMessage.timestamp")?,
                request_id: row.request_id,
                request_doc_id: row.request_doc_id,
            }));
        }
    }

    Ok(None)
}

pub(super) async fn existing_tool_completion_notification(
    node: &EmbeddedNode,
    parent_session_id: &str,
    tool_call_id: &str,
) -> Result<Option<ExistingNotification>> {
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                content
                timestamp
                request_id
                request_doc_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentMessage for background tool completion session={parent_session_id} failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<NotificationMessageRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let needle = format!(
        r#"<tool-completion tool_call_id="{}""#,
        xml_escape_attr(tool_call_id)
    );
    for row in rows {
        if row.content.contains(&needle) {
            return Ok(Some(ExistingNotification {
                sequence: row.sequence,
                timestamp: parse_utc_timestamp(&row.timestamp, "AgentMessage.timestamp")?,
                request_id: row.request_id,
                request_doc_id: row.request_doc_id,
            }));
        }
    }
    Ok(None)
}

pub(super) async fn bound_background_wake_request(
    node: &EmbeddedNode,
    notification: &ExistingNotification,
    queue_key: &str,
) -> Result<Option<String>> {
    let (Some(request_id), Some(request_doc_id)) = (
        notification
            .request_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        notification
            .request_doc_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) else {
        return Ok(None);
    };
    let escaped_doc_id = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
                request_id
                metadata
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query notification-bound AgentRequest {request_doc_id} failed: {:?}",
            response.errors
        );
    }
    let Some(row) = request_rows(response.data.as_ref())?.into_iter().next() else {
        return Ok(None);
    };
    let matches = row.request_id == request_id
        && parse_queue_hints(row.metadata.as_deref()).is_some_and(|hints| {
            hints.source == QueueSource::BackgroundCompletion
                && hints.policy == QueuePolicy::Coalesce
                && hints.key.as_deref() == Some(queue_key)
        });
    Ok(matches.then_some(row.request_id))
}

pub(super) async fn existing_wakeup_after(
    node: &EmbeddedNode,
    parent_session_id: &str,
    queue_key: &str,
    notification_timestamp: &DateTime<Utc>,
) -> Result<Option<String>> {
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                metadata
                created_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query scheduled wake-ups for session {parent_session_id} failed: {:?}",
            response.errors
        );
    }

    let rows = request_rows(response.data.as_ref())?;
    for row in rows {
        let matches_key = parse_queue_hints(row.metadata.as_deref()).is_some_and(|hints| {
            hints.source == QueueSource::BackgroundCompletion
                && hints.policy == QueuePolicy::Coalesce
                && hints.key.as_deref() == Some(queue_key)
        });
        if !matches_key {
            continue;
        }

        let created_at = parse_utc_timestamp(
            row.created_at
                .as_deref()
                .context("AgentRequest.created_at is missing")?,
            "AgentRequest.created_at",
        )?;
        if created_at >= *notification_timestamp {
            return Ok(Some(row.request_id));
        }
    }
    Ok(None)
}

fn request_rows(data: Option<&serde_json::Value>) -> Result<Vec<AgentRequestRow>> {
    let value = data
        .and_then(|data| data.get("AgentRequest"))
        .context("AgentRequest field missing from query response")?;
    serde_json::from_value(value.clone()).context("decode AgentRequest rows")
}

fn parse_utc_timestamp(value: &str, field: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .map_err(|error| anyhow!("{field} is not RFC3339: {error}"))?
        .with_timezone(&Utc))
}
