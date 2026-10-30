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

#[cfg(test)]
pub(super) fn build_pending_turn(
    store: &ClientStore,
    transcript_store: &ClientStore,
    node_did: Option<&str>,
    session_id: &str,
    request_id: &str,
) -> Option<PendingTurnView> {
    let request = store.requests.iter().find(|row| {
        row.request_id == request_id
            && row.session_id.as_deref() == Some(session_id)
            && node_did.is_none_or(|node_did| request_matches_node(row, node_did))
    })?;
    project_pending_turn(store, transcript_store, node_did, session_id, request)
}

/// Project the request selected by this snapshot, not a possibly older replica row.
pub(super) fn project_pending_turn(
    store: &ClientStore,
    transcript_store: &ClientStore,
    node_did: Option<&str>,
    session_id: &str,
    request: &AgentRequestRow,
) -> Option<PendingTurnView> {
    if request.session_id.as_deref() != Some(session_id)
        || node_did.is_some_and(|node_did| !request_matches_node(request, node_did))
    {
        return None;
    }
    if gents::request::lifecycle::retired_by_author(request) {
        return None;
    }
    let request_input = request.input.clone().unwrap_or_default();
    if !gents::request::lifecycle::request_content_owns_user_projection(&request_input) {
        return None;
    }
    let folded_into = gents::request::lifecycle::folded_into(request);

    let lifecycle_state = request
        .lifecycle_state
        .map(|state| state.as_str().to_string());
    let content = normalize_optional(request.content.as_deref())
        .or_else(|| request_input.has_authored_input("").then(String::new))?;
    let owner = gents::request::lifecycle::input_message_owner(request);
    if folded_into.is_some() && owner.is_none() {
        return None;
    }
    let transcript = node_did.map_or_else(
        || transcript_store.transcript(session_id),
        |node_did| transcript_store.transcript_for_node(session_id, node_did),
    );
    if let Some(parent) = normalize_optional(request.retry_parent_request_doc_id.as_deref()) {
        let observed_parent = store.requests.iter().any(|row| {
            row.doc_id.as_deref() == Some(parent.as_str())
                && row.node_did == request.node_did
                && row.requester_did == request.requester_did
                && row.session_id == request.session_id
                && row
                    .input
                    .as_ref()
                    .unwrap_or(&gents_protocol::request_input::RequestInput::default())
                    .has_authored_input(row.content.as_deref().unwrap_or_default())
                && gents::request::lifecycle::request_content_owns_user_projection(
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
        started_goal_id: None,
        request_doc_id: request.doc_id.clone(),
        content: content.to_string(),
        captured_context: (!request_input.captured_context.is_empty())
            .then_some(request_input.captured_context),
        attachments: Some(request_input.attachments.iter().map(|a| a.view()).collect())
            .filter(|v: &Vec<_>| !v.is_empty()),
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
            node_did: Some("node".into()),
            session_id: Some("session".into()),
            content: Some("original instruction".into()),
            retry_parent_request_doc_id: Some("failed-parent".into()),
            ..Default::default()
        });
        assert!(build_pending_turn(&store, &store, Some("node"), "session", "retry").is_some());
        store.requests.push(AgentRequestRow {
            doc_id: Some("failed-parent".into()),
            request_id: "parent".into(),
            node_did: Some("node".into()),
            session_id: Some("session".into()),
            content: Some("original instruction".into()),
            ..Default::default()
        });
        assert!(build_pending_turn(&store, &store, Some("node"), "session", "retry").is_none());
        assert!(build_pending_turn(&store, &store, Some("node"), "session", "parent").is_some());
        store.requests[1].session_id = Some("old-session".into());
        assert!(build_pending_turn(&store, &store, Some("node"), "session", "retry").is_some());
    }

    #[test]
    fn a_message_its_author_retired_is_not_shown() {
        let row =
            |request_id: &str, state, reason: Option<&str>, by: Option<&str>| AgentRequestRow {
                request_id: request_id.into(),
                node_did: Some("node".into()),
                session_id: Some("session".into()),
                content: Some(format!("message {request_id}")),
                lifecycle_state: Some(state),
                failure_reason: reason.map(str::to_owned),
                superseded_by_request: by.map(str::to_owned),
                ..Default::default()
            };
        let mut store = ClientStore::default();
        store.requests.extend([
            row("waiting", RequestLifecycleState::Pending, None, None),
            row(
                "edited",
                RequestLifecycleState::Superseded,
                Some(gents::request::lifecycle::REPLACED_REASON),
                Some("waiting"),
            ),
            row(
                "removed",
                RequestLifecycleState::Interrupted,
                Some(gents::request::lifecycle::REMOVED_REASON),
                None,
            ),
            row("stopped", RequestLifecycleState::Interrupted, None, None),
        ]);
        let shown = |id| build_pending_turn(&store, &store, Some("node"), "session", id).is_some();
        assert!(shown("waiting"));
        assert!(!shown("edited"));
        assert!(!shown("removed"));
        assert!(shown("stopped"));
    }
}
