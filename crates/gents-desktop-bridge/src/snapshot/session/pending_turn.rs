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
    let folded_into = gents::lifecycle::folded_into(request);

    let lifecycle_state = request
        .lifecycle_state
        .map(|state| state.as_str().to_string());
    let content = normalize_optional(request.content.as_deref())?;
    // A folded row is consumed in the transaction that publishes its entry
    // under the claiming request; a replica can hold the row first.
    let owner = gents::lifecycle::input_message_owner(request);
    if folded_into.is_some() && owner.is_none() {
        return None;
    }
    // Pending ownership is session state, not visible-page state. A materialized
    // user row outside the current window must still suppress the request-owned
    // placeholder at the tip.
    let transcript = agent_did.map_or_else(
        || transcript_store.transcript(session_id),
        |agent_did| transcript_store.transcript_for_agent(session_id, agent_did),
    );
    if let Some(parent) = normalize_optional(request.retry_parent_request_doc_id.as_deref()) {
        let observed_parent = store.requests.iter().any(|row| {
            row.doc_id.as_deref() == Some(parent.as_str())
                && row.agent_did == request.agent_did
                && row.requester_did == request.requester_did
                && row.session_id == request.session_id
                && normalize_optional(row.content.as_deref()).is_some()
                && gents::lifecycle::request_content_owns_user_projection(
                    &row.input.clone().unwrap_or_default(),
                )
        });
        let parent_key = authored_prompt_message_key(&parent);
        let published_parent = transcript.messages.iter().any(|row| {
            row.message.request_doc_id.as_deref() == Some(parent.as_str())
                && row.message.requester_did == request.requester_did
                && row.message.role == gents_protocol::output::MessageRole::User
                && row.message.message_key == parent_key
        });
        if observed_parent || published_parent {
            return None;
        }
    }
    let exact_owner = owner.is_some_and(|(owner_doc_id, owner_key)| {
        transcript.messages.iter().any(|row| {
            row.message.request_doc_id.as_deref() == Some(owner_doc_id.as_str())
                && row.message.role == gents_protocol::output::MessageRole::User
                && row.message.message_key == owner_key
        })
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
        folded_into_request_id: folded_into.map(str::to_owned),
        origin: super::request_origin_view(store, request),
        created_at: normalize_optional(request.created_at.as_deref()),
    })
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn retry_does_not_add_a_pending_copy_of_the_user_message() {
        let mut store = ClientStore::default();
        store.requests.push(AgentRequestRow {
            request_id: "retry".into(),
            agent_did: Some("agent".into()),
            session_id: Some("session".into()),
            content: Some("original instruction".into()),
            retry_parent_request_doc_id: Some("failed-parent".into()),
            ..Default::default()
        });
        assert!(build_pending_turn(&store, &store, Some("agent"), "session", "retry").is_some());
        store.requests.push(AgentRequestRow {
            doc_id: Some("failed-parent".into()),
            request_id: "parent".into(),
            agent_did: Some("agent".into()),
            session_id: Some("session".into()),
            content: Some("original instruction".into()),
            ..Default::default()
        });
        assert!(build_pending_turn(&store, &store, Some("agent"), "session", "retry").is_none());
        assert!(build_pending_turn(&store, &store, Some("agent"), "session", "parent").is_some());
        store.requests[1].session_id = Some("old-session".into());
        assert!(build_pending_turn(&store, &store, Some("agent"), "session", "retry").is_some());
    }
}
