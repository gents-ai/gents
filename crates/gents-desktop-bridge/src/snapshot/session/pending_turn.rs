use super::*;

pub(super) fn project_retry_eligibility(request: Option<&AgentRequestRow>) -> RetryEligibilityView {
    let Some(request) = request else {
        return RetryEligibilityView {
            eligible: false,
            denial_reason: Some("requestNotObserved".to_string()),
        };
    };
    if request.lifecycle_state.as_deref() != Some(RequestLifecycleState::Failed.as_str()) {
        return RetryEligibilityView {
            eligible: false,
            denial_reason: Some("notFailed".to_string()),
        };
    }
    if request.execution_origin.as_deref() != Some("interactive") {
        return RetryEligibilityView {
            eligible: false,
            denial_reason: Some("nonInteractiveOrigin".to_string()),
        };
    }
    if request.retry_count.unwrap_or_default() >= request.max_retries.unwrap_or(3) {
        return RetryEligibilityView {
            eligible: false,
            denial_reason: Some("retryBudgetExhausted".to_string()),
        };
    }
    if let Some(deadline) = normalize_optional(request.deadline.as_deref()) {
        let Ok(deadline) = DateTime::parse_from_rfc3339(&deadline) else {
            return RetryEligibilityView {
                eligible: false,
                denial_reason: Some("invalidDeadline".to_string()),
            };
        };
        if Utc::now() > deadline.with_timezone(&Utc) {
            return RetryEligibilityView {
                eligible: false,
                denial_reason: Some("deadlineClosed".to_string()),
            };
        }
    }
    RetryEligibilityView {
        eligible: true,
        denial_reason: None,
    }
}

fn selected_skill_ids_from_metadata(metadata: Option<&str>) -> Vec<String> {
    let Some(metadata) = metadata.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return Vec::new();
    };

    value
        .get("selected_skill_ids")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn build_pending_turn(
    store: &ClientStore,
    transcript_store: &ClientStore,
    agent_did: Option<&str>,
    session_id: &str,
    request_id: &str,
) -> Option<PendingTurnView> {
    let request = store.requests.iter().find(|row| {
        row.request_id == request_id
            && row.session_id.as_deref() == Some(session_id)
            && agent_did.is_none_or(|agent_did| request_matches_agent(row, agent_did, false))
    })?;
    if !gents::lifecycle::request_content_owns_user_projection(request.metadata.as_deref()) {
        return None;
    }

    let lifecycle_state = normalize_optional(request.lifecycle_state.as_deref());
    let content = normalize_optional(request.content.as_deref())?;
    // Pending ownership is session state, not visible-page state. A materialized
    // user row outside the current window must still suppress the request-owned
    // placeholder at the tip.
    let transcript = agent_did.map_or_else(
        || transcript_store.transcript(session_id),
        |agent_did| transcript_store.transcript_for_agent(session_id, agent_did),
    );
    let requests_by_id = store
        .requests
        .iter()
        .map(|request| (request.request_id.as_str(), request))
        .collect::<HashMap<_, _>>();
    let keyed_steering_request_ids = keyed_steering_request_ids(&transcript.messages);
    let messages = transcript
        .messages
        .into_iter()
        .map(|row| {
            let role = normalize_optional(row.role.as_deref());
            let body = normalize_optional(row.content.as_deref());
            let presentation = role
                .as_deref()
                .zip(body.as_deref())
                .map(|(role, content)| present_persisted_message(role, content));

            MessageView {
                message_key: row.message_key.clone(),
                request_id: row.request_id.clone(),
                sequence: row.sequence,
                role,
                content: body,
                display_role: presentation
                    .as_ref()
                    .map(|presentation| presentation.role.label().to_ascii_lowercase()),
                display_content: presentation.as_ref().and_then(|presentation| {
                    normalize_optional(Some(presentation.body_markdown.as_str()))
                }),
                reasoning: None,
                has_tool_calls: false,
                has_tool_results: false,
                runtime_control: message_is_runtime_control(
                    row,
                    &requests_by_id,
                    &keyed_steering_request_ids,
                ),
                timestamp: normalize_optional(row.timestamp.as_deref()),
            }
        })
        .collect::<Vec<_>>();

    let exact_owner = has_materialized_user_owner(&messages, request_id);
    if exact_owner {
        return None;
    }

    Some(PendingTurnView {
        request_id: request.request_id.clone(),
        content: content.to_string(),
        selected_skill_ids: selected_skill_ids_from_metadata(request.metadata.as_deref()),
        lifecycle_state,
        created_at: normalize_optional(request.created_at.as_deref()),
    })
}
