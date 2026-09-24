//! Run generated Codex client-status vectors through the protocol owner while
//! the full runtime crate is intentionally red.
//!
//! This does not model local interrupt acknowledgement: that is a shim-local,
//! transient observation and is deliberately outside the persisted request
//! projection owned by `gents_protocol::client_protocol`.
#[path = "../../gents/src/lean_vocab_test/support.rs"]
mod runtime_contract;

use std::collections::BTreeSet;

use gents_protocol::client_protocol::{
    project_persisted_attempt, ClientHeadProjection, ClientTurnState, RequestLifecycleState,
};

fn codex_phase(state: ClientTurnState) -> &'static str {
    match state {
        ClientTurnState::WaitingForClaim | ClientTurnState::Running => "inProgress",
        ClientTurnState::Completed => "completed",
        ClientTurnState::Failed => "failed",
        ClientTurnState::Superseded | ClientTurnState::Interrupted => "interrupted",
    }
}

fn subagent_status(projection: ClientHeadProjection) -> (&'static str, bool) {
    match (projection.turn_state, projection.request_state) {
        (ClientTurnState::Completed, _) => ("completed", true),
        (ClientTurnState::Failed, _) => ("errored", true),
        (ClientTurnState::Superseded | ClientTurnState::Interrupted, _) => ("interrupted", true),
        (ClientTurnState::WaitingForClaim, RequestLifecycleState::Pending) => {
            ("pendingInit", false)
        }
        (ClientTurnState::WaitingForClaim | ClientTurnState::Running, _) => ("running", false),
    }
}

fn thread_status(state: Option<ClientTurnState>) -> &'static str {
    match state {
        Some(ClientTurnState::WaitingForClaim | ClientTurnState::Running) => "active",
        Some(ClientTurnState::Failed) => "systemError",
        Some(
            ClientTurnState::Completed | ClientTurnState::Superseded | ClientTurnState::Interrupted,
        )
        | None => "idle",
    }
}

#[test]
fn generated_codex_client_status_vectors_use_persisted_request_projection() {
    let snapshot: runtime_contract::LeanContractSnapshot =
        gents_lean_contract::load_contract_snapshot().expect("decode current Lean contracts");

    let persisted_cases = snapshot
        .codex_shim_projection_cases
        .iter()
        .filter(|case| !case.local_interrupt_acked)
        .collect::<Vec<_>>();
    let all_lifecycle_states = RequestLifecycleState::ALL
        .into_iter()
        .map(RequestLifecycleState::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        persisted_cases
            .iter()
            .map(|case| case.request_state.as_str())
            .collect::<BTreeSet<_>>(),
        all_lifecycle_states,
        "generated persisted Codex vectors must cover every lifecycle state"
    );
    assert!(persisted_cases.iter().any(|case| case.is_superseded));
    for case in persisted_cases {
        let projection = project_persisted_attempt(&case.request_state, case.is_superseded)
            .unwrap_or_else(|| panic!("{}: invalid persisted lifecycle", case.witness));
        assert_eq!(
            case.projected_phase,
            codex_phase(projection.turn_state),
            "{}",
            case.witness
        );
        assert_eq!(case.terminal, projection.is_terminal(), "{}", case.witness);
        assert_eq!(
            case.effectively_terminal,
            projection.is_terminal(),
            "{}",
            case.witness
        );
    }

    // `local_interrupt_acked` is intentionally not passed to the protocol
    // owner: it has no persisted request-lifecycle representation.
    assert!(snapshot
        .codex_shim_projection_cases
        .iter()
        .any(|case| case.local_interrupt_acked));

    let subagent_cases = &snapshot.codex_shim_subagent_status_cases;
    assert!(!subagent_cases.is_empty());
    assert_eq!(
        subagent_cases
            .iter()
            .map(|case| case.request_state.as_str())
            .collect::<BTreeSet<_>>(),
        all_lifecycle_states,
        "generated subagent vectors must cover every lifecycle state"
    );
    for case in subagent_cases {
        let projection = project_persisted_attempt(&case.request_state, false)
            .unwrap_or_else(|| panic!("{}: invalid persisted lifecycle", case.witness));
        let expected = subagent_status(projection);
        assert_eq!(case.projected_agent_status, expected.0, "{}", case.witness);
        assert_eq!(case.terminal, expected.1, "{}", case.witness);
    }

    let thread_cases = &snapshot.codex_shim_thread_status_cases;
    assert!(!thread_cases.is_empty());
    assert_eq!(
        thread_cases
            .iter()
            .filter_map(|case| case.request_state.as_deref())
            .collect::<BTreeSet<_>>(),
        all_lifecycle_states,
        "generated thread vectors must cover every lifecycle state"
    );
    for case in thread_cases {
        let state = case.request_state.as_deref().map(|request_state| {
            project_persisted_attempt(request_state, false)
                .unwrap_or_else(|| panic!("{}: invalid persisted lifecycle", case.witness))
                .turn_state
        });
        assert_eq!(
            case.projected_status,
            thread_status(state),
            "{}",
            case.witness
        );
    }
}
