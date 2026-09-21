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
