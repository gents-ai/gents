use super::*;

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

    anyhow::ensure!(
        parent_request.session_id == parent_session_id,
        "background completion parent session mismatch"
    );
    let existing = existing_notification(node, parent_session_id, &edge.child_request_id).await?;
    let notification = render_notification(edge, status, summary);
    let key = background_completion_notification_message_key(&edge.child_request_id, "subagent");
    notification_delivery::ensure_notification_delivery(
        node,
        &parent_request,
        existing,
        &notification,
        &key,
    )
    .await
}

pub(super) fn bridge_state_is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "timedOut" | "cancelled")
}

pub(super) struct ExistingNotification {
    pub(super) doc_id: String,
}

#[derive(Debug, Deserialize)]
struct NotificationMessageRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    content: String,
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
                _docID
                content
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
            return Ok(Some(ExistingNotification { doc_id: row.doc_id }));
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
                _docID
                content
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
            return Ok(Some(ExistingNotification { doc_id: row.doc_id }));
        }
    }
    Ok(None)
}
