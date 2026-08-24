use super::*;

pub(super) async fn ensure_projection_side_effects(
    node: &EmbeddedNode,
    _parent_session_id: &str,
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

    let notification = render_notification(edge, status, summary);
    let notification_message_key =
        background_completion_notification_message_key(&edge.child_request_id, "subagent");
    let enqueued = enqueue_conversation_continuation(
        node,
        &parent_request,
        ConversationContinuation::BackgroundCompletion {
            notification: &notification,
            notification_key: &notification_message_key,
            queued_after_request_id: parent_request_id,
        },
    )
    .await?;

    let notification_sequence = enqueued.input_sequence.ok_or_else(|| {
        anyhow!("background completion continuation returned no notification sequence")
    })?;
    Ok(SideEffects {
        notification_sequence,
        wake_request_id: enqueued.request.request_id,
        created_notification: enqueued.created_input,
        created_wake: enqueued.created_request,
    })
}

pub(super) fn bridge_state_is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "timedOut" | "cancelled")
}
