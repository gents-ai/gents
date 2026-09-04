use super::*;

pub async fn project_background_subagent_completion(
    node: Arc<EmbeddedNode>,
    child_request_id: &str,
    local_did: &str,
) -> Result<BackgroundCompletionOutcome> {
    project_background_subagent_completion_inner(
        node.as_ref(),
        Some(node.clone()),
        child_request_id,
        local_did,
    )
    .await
}

pub(crate) async fn ensure_background_subagent_completion_side_effects(
    node: &EmbeddedNode,
    child_request_id: &str,
    local_did: &str,
) -> Result<BackgroundCompletionOutcome> {
    project_background_subagent_completion_inner(node, None, child_request_id, local_did).await
}

async fn project_background_subagent_completion_inner(
    node: &EmbeddedNode,
    lifecycle_node: Option<Arc<EmbeddedNode>>,
    child_request_id: &str,
    local_did: &str,
) -> Result<BackgroundCompletionOutcome> {
    let Some(linkage) = load_child_linkage(node, child_request_id).await? else {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    };
    let Some(parent_request_id) = non_empty(linkage.caused_by_parent_request_id.as_deref()) else {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    };
    let Some(parent_request_doc_id) = non_empty(linkage.caused_by_parent_request_doc_id.as_deref())
    else {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    };
    if non_empty(linkage.caused_by_parent_tool_call_id.as_deref()).is_none()
        || non_empty(linkage.caused_by_parent_tool_call_doc_id.as_deref()).is_none()
    {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    }
    if !request_is_locally_owned(node, parent_request_id, parent_request_doc_id, local_did).await? {
        return Ok(BackgroundCompletionOutcome::NotLocalOwner);
    }

    let Some(terminal_row) = load_child_terminal_row(node, child_request_id).await? else {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    };
    let completed = child_request_completed(&terminal_row);
    let terminal = if completed {
        None
    } else {
        let Some(terminal) = project_child_terminal(&terminal_row) else {
            return Ok(BackgroundCompletionOutcome::NotTerminal);
        };
        Some(terminal)
    };

    let parent_context = load_parent_subagent_context(node, parent_request_id).await?;
    let edge = load_authorized_child_edge(node, &parent_context, child_request_id).await?;
    if edge.await_mode != AwaitMode::Background {
        return Ok(BackgroundCompletionOutcome::NotBackground);
    }

    let (status, summary, bridge_result, terminal) = if completed {
        let Some(final_response) =
            load_projected_final_response(node, &parent_context.session_id, &edge).await?
        else {
            return Ok(BackgroundCompletionOutcome::MissingFinalResponse);
        };
        let summary = compact_summary(&final_response);
        ("completed".to_string(), summary, Some(final_response), None)
    } else {
        let terminal = terminal.expect("non-completed child terminal checked above");
        let status = child_terminal_status(&terminal).to_string();
        let (reason, _failure_class) = child_terminal_reason(&terminal);
        let summary = compact_summary(&reason);
        (status, summary, None, Some(terminal))
    };

    let mut transitioned = false;
    if edge.lifecycle_state == "running" {
        let Some(lifecycle_node) = lifecycle_node else {
            return Ok(BackgroundCompletionOutcome::NotTerminal);
        };
        let mut lifecycle = match ToolCallLifecycle::load(
            lifecycle_node,
            &parent_context.session_id,
            &edge.parent_tool_call_id,
        )
        .await?
        {
            Some(lifecycle) => lifecycle,
            None => return Ok(BackgroundCompletionOutcome::Unlinked),
        };

        transitioned = match (bridge_result.clone(), terminal.clone()) {
            (Some(final_response), None) => lifecycle.bridge_complete(final_response).await?,
            (None, Some(terminal)) => lifecycle.bridge_failure(terminal).await?,
            _ => false,
        };
    } else if !bridge_state_is_terminal(&edge.lifecycle_state) {
        return Ok(BackgroundCompletionOutcome::AlreadyProjected);
    }

    let side_effects = ensure_projection_side_effects(
        node,
        &parent_context.session_id,
        &parent_context.request_id,
        &edge,
        &status,
        &summary,
    )
    .await?;

    let outcome = if transitioned || side_effects.created_notification || side_effects.created_wake
    {
        BackgroundCompletionOutcome::Projected {
            child_request_id: edge.child_request_id,
            parent_request_id: parent_context.request_id,
            parent_tool_call_id: edge.parent_tool_call_id,
            parent_session_id: parent_context.session_id,
            notification_sequence: side_effects.notification_sequence,
            wake_request_id: side_effects.wake_request_id,
        }
    } else {
        BackgroundCompletionOutcome::AlreadyProjected
    };
    Ok(outcome)
}

async fn load_projected_final_response(
    node: &EmbeddedNode,
    parent_session_id: &str,
    edge: &ChildEdge,
) -> Result<Option<String>> {
    if let Some(final_response) = load_child_final_response(node, edge).await? {
        return Ok(Some(final_response));
    }
    if edge.lifecycle_state == "completed" {
        return match session::load_tool_call_result(
            node,
            parent_session_id,
            &edge.parent_tool_call_id,
        )
        .await
        {
            Ok(result) if !result.trim().is_empty() => Ok(Some(result)),
            Ok(_) => Ok(None),
            Err(error) => Err(error),
        };
    }
    Ok(None)
}
