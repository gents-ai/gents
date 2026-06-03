use super::*;

pub(super) fn generated_codex_shim_projection_cases_pin_adapter_mapping() {
    let cases = lean_codex_shim_projection_cases();
    assert_eq!(cases.len(), 12);

    for case in cases {
        assert_eq!(
            case.terminal, case.effectively_terminal,
            "{} should satisfy CodexShim.codex_turn_terminates_precisely",
            case.witness
        );
        assert!(
            case.lean_theorems
                .contains(&"CodexShim.codex_turn_terminates_precisely".to_string()),
            "{} should cite terminal coherence",
            case.witness
        );
        if case.local_interrupt_acked {
            assert!(
                case.interruptible_request_state,
                "{} must only acknowledge local interrupts for interruptible states",
                case.witness
            );
            assert!(
                case.lean_theorems
                    .contains(&"CodexShim.local_interrupt_requires_interruptible".to_string()),
                "{} should cite local interrupt eligibility",
                case.witness
            );
            assert!(
                case.lean_theorems
                    .contains(&"CodexShim.local_interrupt_shortcut_sound".to_string()),
                "{} should cite local interrupt soundness",
                case.witness
            );
        }
    }

    let names = cases
        .iter()
        .map(|case| case.witness.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "codex_shim.projection.pending_no_response",
            "codex_shim.projection.claimed_no_response",
            "codex_shim.projection.processing_streaming_response",
            "codex_shim.projection.nonterminal_complete_response",
            "codex_shim.projection.nonterminal_error_response",
            "codex_shim.projection.completed_request",
            "codex_shim.projection.failed_request",
            "codex_shim.projection.dead_request",
            "codex_shim.projection.superseded_request",
            "codex_shim.projection.interrupted_request",
            "codex_shim.projection.local_interrupt_preempts_core_state",
            "codex_shim.projection.local_interrupt_input_required",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    let pending = lean_codex_shim_projection_case("codex_shim.projection.pending_no_response");
    assert_eq!(pending.request_state, "pending");
    assert_eq!(pending.response_status, None);
    assert!(!pending.local_interrupt_acked);
    assert_eq!(pending.projected_phase, "inProgress");
    assert!(!pending.terminal);
    assert!(!pending.effectively_terminal);
    assert!(!pending.interruptible_request_state);
    assert_eq!(
        pending.lean_theorems,
        vec![
            "CodexShim.project_pending_is_in_progress".to_string(),
            "CodexShim.nonterminal_without_response_projects_in_progress".to_string(),
            "CodexShim.request_transition_projection_monotonic".to_string(),
            "CodexShim.codex_turn_terminates_precisely".to_string(),
        ]
    );

    let completed = lean_codex_shim_projection_case("codex_shim.projection.completed_request");
    assert_eq!(completed.request_state, "completed");
    assert_eq!(completed.response_status.as_deref(), Some("error"));
    assert!(!completed.local_interrupt_acked);
    assert_eq!(completed.projected_phase, "completed");
    assert!(completed.terminal);
    assert!(completed.effectively_terminal);
    assert!(!completed.interruptible_request_state);
    assert!(completed
        .lean_theorems
        .contains(&"CodexShim.terminal_request_overrides_response".to_string()));

    let local_interrupt = lean_codex_shim_projection_case(
        "codex_shim.projection.local_interrupt_preempts_core_state",
    );
    assert_eq!(local_interrupt.request_state, "processing");
    assert_eq!(
        local_interrupt.response_status.as_deref(),
        Some("streaming")
    );
    assert!(local_interrupt.local_interrupt_acked);
    assert_eq!(local_interrupt.projected_phase, "interrupted");
    assert!(local_interrupt.terminal);
    assert!(local_interrupt.effectively_terminal);
    assert!(local_interrupt.interruptible_request_state);
    assert_eq!(
        local_interrupt.lean_theorems,
        vec![
            "CodexShim.local_interrupt_projects_interrupted".to_string(),
            "CodexShim.local_interrupt_never_projects_in_progress".to_string(),
            "CodexShim.codex_turn_terminates_precisely".to_string(),
            "CodexShim.local_interrupt_requires_interruptible".to_string(),
            "CodexShim.local_interrupt_shortcut_sound".to_string(),
        ]
    );

    let input_required =
        lean_codex_shim_projection_case("codex_shim.projection.local_interrupt_input_required");
    assert_eq!(input_required.request_state, "inputRequired");
    assert!(input_required.local_interrupt_acked);
    assert!(input_required.interruptible_request_state);
    assert_eq!(input_required.projected_phase, "interrupted");

    let lifecycle_cases = lean_codex_shim_turn_lifecycle_cases();
    assert_eq!(lifecycle_cases.len(), 4);
    let lifecycle_names = lifecycle_cases
        .iter()
        .map(|case| case.witness.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        lifecycle_names,
        [
            "codex_shim.turn_lifecycle.start",
            "codex_shim.turn_lifecycle.complete",
            "codex_shim.turn_lifecycle.fail",
            "codex_shim.turn_lifecycle.interrupt",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );
    for case in lifecycle_cases {
        assert!(
            case.monotonic,
            "{} should be marked monotonic by CodexShim.turn_lifecycle_never_regresses",
            case.witness
        );
        assert!(
            case.post_lex_ord >= case.pre_lex_ord,
            "{} must not regress in TurnPhase.lexOrd",
            case.witness
        );
        assert!(
            case.lean_theorems
                .contains(&"CodexShim.turn_lifecycle_never_regresses".to_string()),
            "{} should cite turn lifecycle monotonicity",
            case.witness
        );
    }
    let interrupt = lifecycle_cases
        .iter()
        .find(|case| case.witness == "codex_shim.turn_lifecycle.interrupt")
        .expect("interrupt lifecycle witness");
    assert_eq!(interrupt.action, "interrupt");
    assert_eq!(interrupt.pre_phase, "inProgress");
    assert_eq!(interrupt.post_phase, "interrupted");
    assert!(interrupt
        .lean_theorems
        .contains(&"CodexShim.interrupt_step_is_terminal".to_string()));
}
