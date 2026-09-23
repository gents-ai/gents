use super::*;

pub(super) fn project_retry_eligibility(request: Option<&AgentRequestRow>) -> RetryEligibilityView {
    let Some(request) = request else {
        return RetryEligibilityView {
            eligible: false,
            denial_reason: Some("requestNotObserved".to_string()),
        };
    };
    if request.lifecycle_state != Some(RequestLifecycleState::Failed) {
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
            && agent_did.is_none_or(|agent_did| request_matches_agent(row, agent_did))
    })?;
    let request_input = request.input.clone().unwrap_or_default();
    if !gents::lifecycle::request_content_owns_user_projection(&request_input) {
        return None;
    }

    let lifecycle_state = request
        .lifecycle_state
        .map(|state| state.as_str().to_string());
    let content = normalize_optional(request.content.as_deref())?;
    let request_doc_id = request.doc_id.as_deref();
    // Pending ownership is session state, not visible-page state. A materialized
    // user row outside the current window must still suppress the request-owned
    // placeholder at the tip.
    let transcript = agent_did.map_or_else(
        || transcript_store.transcript(session_id),
        |agent_did| transcript_store.transcript_for_agent(session_id, agent_did),
    );
    let prompt_message_key = request_doc_id.map(|doc_id| format!("authored:{doc_id}:prompt"));
    let exact_owner = transcript.messages.iter().any(|row| {
        request_doc_id.is_some()
            && row.message.request_doc_id.as_deref() == request_doc_id
            && row.message.role == gents_protocol::output::MessageRole::User
            && prompt_message_key.as_deref() == Some(row.message.message_key.as_str())
    });
    if exact_owner {
        return None;
    }

    Some(PendingTurnView {
        request_id: request.request_id.clone(),
        request_doc_id: request.doc_id.clone(),
        content: content.to_string(),
        selected_skill_ids: request_input.selected_skill_ids,
        lifecycle_state,
        created_at: normalize_optional(request.created_at.as_deref()),
    })
}
