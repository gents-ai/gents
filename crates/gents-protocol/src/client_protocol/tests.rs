use super::*;

fn attempt(lifecycle_state: RequestLifecycleState, is_superseded: bool) -> AttemptView {
    AttemptView {
        request: RequestSnapshot {
            request_id: "request".into(),
            retry_parent_request: None,
            lifecycle_state,
            is_superseded,
        },
    }
}

fn keyed_attempt(
    request_id: &str,
    retry_parent_request: Option<&str>,
    lifecycle_state: RequestLifecycleState,
    is_superseded: bool,
) -> AttemptView {
    AttemptView {
        request: RequestSnapshot {
            request_id: request_id.into(),
            retry_parent_request: retry_parent_request.map(str::to_string),
            lifecycle_state,
            is_superseded,
        },
    }
}

#[test]
fn projection_preserves_request_detail_and_terminality() {
    for state in RequestLifecycleState::ALL {
        let head = project_attempt(&attempt(state, false));
        assert_eq!(head.request_state, state);
        assert_eq!(head.is_terminal(), state.is_terminal());
        assert_eq!(head.is_active(), !state.is_terminal());
        assert_eq!(
            head.waiting_on_user_input(),
            state == RequestLifecycleState::InputRequired
        );
    }
}

#[test]
fn persisted_attempt_takes_exactly_two_arguments() {
    for state in RequestLifecycleState::ALL {
        let derived = derive_persisted_attempt(state.as_str(), false);
        assert_eq!(derived, Some(derive_attempt(&attempt(state, false))));

        let projected = project_persisted_attempt(state.as_str(), false);
        assert_eq!(projected, Some(project_attempt(&attempt(state, false))));
    }
}

#[test]
fn persisted_attempt_supersession_matches_in_memory_projection() {
    for state in RequestLifecycleState::ALL {
        assert_eq!(
            derive_persisted_attempt(state.as_str(), true),
            Some(ClientTurnState::Superseded)
        );
    }
}

#[test]
fn persisted_attempt_rejects_invalid_lifecycle_strings() {
    assert_eq!(derive_persisted_attempt("", false), None);
    assert_eq!(derive_persisted_attempt("running", false), None);
    assert_eq!(project_persisted_attempt("", false), None);
    assert_eq!(project_persisted_attempt("running", false), None);
}

#[test]
fn unordered_turn_resolves_the_single_unreferenced_tip() {
    let attempts = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r2", Some("r1"), RequestLifecycleState::Failed, false),
        keyed_attempt("r3", Some("r2"), RequestLifecycleState::Processing, false),
    ];
    // Order is irrelevant: the tip is the one candidate not referenced as a
    // parent, not the last or first element.
    assert_eq!(derive_turn(&attempts), Some(ClientTurnState::Running));

    let reversed: Vec<_> = attempts.into_iter().rev().collect();
    assert_eq!(derive_turn(&reversed), Some(ClientTurnState::Running));
}

#[test]
fn unordered_turn_accepts_tip_whose_parent_is_in_scope() {
    // The tip's own retry parent being present in the candidate set is the
    // normal chain shape, not a cycle.
    let attempts = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r2", Some("r1"), RequestLifecycleState::Completed, false),
    ];
    assert_eq!(derive_turn(&attempts), Some(ClientTurnState::Completed));
}

#[test]
fn unordered_turn_rejects_empty_and_duplicate_ids() {
    assert_eq!(derive_turn(&[]), None);

    let duplicated = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
    ];
    assert_eq!(derive_turn(&duplicated), None);
}

#[test]
fn unordered_turn_rejects_parent_cycle() {
    let cycle = [
        keyed_attempt("r1", Some("r2"), RequestLifecycleState::Completed, false),
        keyed_attempt("r2", Some("r1"), RequestLifecycleState::Completed, false),
    ];
    assert_eq!(derive_turn(&cycle), None);
}

#[test]
fn unordered_turn_rejects_ambiguous_tips() {
    let ambiguous = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r2", None, RequestLifecycleState::Failed, false),
    ];
    assert_eq!(derive_turn(&ambiguous), None);
}

#[test]
fn supersession_overrides_every_lifecycle() {
    for state in RequestLifecycleState::ALL {
        let head = project_attempt(&attempt(state, true));
        assert_eq!(head.turn_state, ClientTurnState::Superseded);
        assert_eq!(head.request_state, state);
        assert!(!head.waiting_on_user_input());
    }
}

#[test]
fn claimed_and_silent_processing_are_running_without_response_facts() {
    for state in [
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
        RequestLifecycleState::InputRequired,
    ] {
        assert_eq!(
            derive_attempt(&attempt(state, false)),
            ClientTurnState::Running
        );
    }
}
