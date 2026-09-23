use super::*;

pub(super) async fn ensure_projection_side_effects(
    node: &EmbeddedNode,
    parent_session_id: &str,
    parent_request_id: &str,
    edge: &ChildEdge,
    status: &str,
    summary: &str,
    bridge_source: &str,
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
    let key = background_completion_notification_message_key(&edge.child_request_id, "subagent");
    let existing = existing_notification(node, &parent_request, &key).await?;
    let (notification, presentation) =
        subagent_notification_presentation(edge, status, summary, bridge_source);
    notification_delivery::ensure_notification_delivery(
        node,
        &parent_request,
        existing,
        &notification,
        &key,
        Some(crate::lifecycle::queue::ToolNotificationPublication {
            tool_call_doc_id: edge.parent_tool_call_doc_id.clone(),
            presentation,
        }),
    )
    .await
}

pub(super) fn bridge_state_is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "timedOut" | "cancelled")
}

pub(super) struct ExistingNotification {
    pub(super) doc_id: String,
}

async fn existing_notification(
    node: &EmbeddedNode,
    parent: &crate::AgentRequest,
    message_key: &str,
) -> Result<Option<ExistingNotification>> {
    let scope = crate::session::session_scope_filter(
        &parent.agent_did,
        &parent.session_id,
        parent.requester_did.as_deref(),
    );
    let message_key = escape_graphql_string(message_key);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ {scope}, message_key: {{ _eq: "{message_key}" }} }},
                limit: 2
            ) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent canonical notification key {message_key} failed: {:?}",
            response.errors
        );
    }
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "AgentMessage")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "canonical notification key {message_key} is ambiguous"
    );
    match rows.as_slice() {
        [] => Ok(None),
        [row] => {
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .filter(|doc_id| !doc_id.is_empty())
                .context("canonical notification key matched a row without _docID")?;
            Ok(Some(ExistingNotification {
                doc_id: doc_id.to_owned(),
            }))
        }
        _ => unreachable!("duplicate canonical notification rows were rejected above"),
    }
}

pub(super) async fn existing_tool_completion_notification(
    node: &EmbeddedNode,
    parent: &crate::AgentRequest,
    tool_call_id: &str,
) -> Result<Option<ExistingNotification>> {
    let key = background_completion_notification_message_key(tool_call_id, "tool");
    existing_notification(node, parent, &key).await
}
