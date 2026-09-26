use super::*;

pub(super) fn generated_codex_shim_projection_cases_pin_adapter_mapping() {
    let cases = lean_codex_shim_projection_cases();
    assert_eq!(cases.len(), 11);

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
        // `is_superseded` is an independent observation input, not a derived
        // alias for the lifecycle state. The model exercises both a superseded
        // lifecycle with no override and a processing lifecycle with one.
        // Compare their native projection below, without rewriting either input.
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
        } else {
            // The exported phase must agree with the canonical client
            // projection (`gents_protocol::client_protocol`), which owns
            // `deriveAttempt`/`ClientTurnState` in Rust. The local-interrupt
            // override is shim-specific and is checked by the dedicated
            // witnesses below.
            let head = gents_protocol::client_protocol::project_persisted_attempt(
                &case.request_state,
                case.is_superseded,
            )
            .unwrap_or_else(|| panic!("{}: invalid lifecycle vocabulary", case.witness));
            use gents_protocol::client_protocol::ClientTurnState;
            let phase = match head.turn_state {
                ClientTurnState::WaitingForClaim => "inProgress",
                ClientTurnState::Running => "inProgress",
                ClientTurnState::Completed => "completed",
                ClientTurnState::Failed => "failed",
                ClientTurnState::Superseded | ClientTurnState::Interrupted => "interrupted",
            };
            assert_eq!(
                case.projected_phase, phase,
                "{}: exported phase must match the canonical client projection",
                case.witness
            );
            assert_eq!(
                case.terminal,
                head.turn_state.is_terminal(),
                "{}: exported terminality must match the canonical client projection",
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
            "codex_shim.projection.workspace_binding_pending",
            "codex_shim.projection.pending",
            "codex_shim.projection.claimed",
            "codex_shim.projection.processing",
            "codex_shim.projection.completed_request",
            "codex_shim.projection.failed_request",
            "codex_shim.projection.dead_request",
            "codex_shim.projection.superseded_request",
            "codex_shim.projection.supersession_override",
            "codex_shim.projection.interrupted_request",
            "codex_shim.projection.local_interrupt_preempts_core_state",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    let pending = lean_codex_shim_projection_case("codex_shim.projection.pending");
    assert_eq!(pending.request_state, "pending");
    assert!(!pending.is_superseded);
    assert!(!pending.local_interrupt_acked);
    assert_eq!(pending.projected_phase, "inProgress");
    assert!(!pending.terminal);
    assert!(!pending.effectively_terminal);
    assert!(!pending.interruptible_request_state);
    assert!(pending
        .lean_theorems
        .contains(&"deriveAttempt_total".to_string()));
    assert!(pending
        .lean_theorems
        .contains(&"lifecycle_transition_monotonic".to_string()));
    assert!(pending
        .lean_theorems
        .contains(&"terminal_coherence".to_string()));
    assert!(pending
        .lean_theorems
        .contains(&"CodexShim.projectClientTurnState_terminal".to_string()));
    assert!(pending
        .lean_theorems
        .contains(&"CodexShim.projection_without_local_interrupt".to_string()));
    assert!(pending
        .lean_theorems
        .contains(&"CodexShim.codex_turn_terminates_precisely".to_string()));

    let completed = lean_codex_shim_projection_case("codex_shim.projection.completed_request");
    assert_eq!(completed.request_state, "completed");
    assert!(!completed.is_superseded);
    assert!(!completed.local_interrupt_acked);
    assert_eq!(completed.projected_phase, "completed");
    assert!(completed.terminal);
    assert!(completed.effectively_terminal);
    assert!(!completed.interruptible_request_state);
    assert!(completed
        .lean_theorems
        .contains(&"terminal_coherence".to_string()));

    let supersession =
        lean_codex_shim_projection_case("codex_shim.projection.supersession_override");
    assert_eq!(supersession.request_state, "processing");
    assert!(supersession.is_superseded);
    assert!(!supersession.local_interrupt_acked);
    assert_eq!(supersession.projected_phase, "interrupted");
    assert!(supersession.terminal);
    assert!(supersession.effectively_terminal);

    let local_interrupt = lean_codex_shim_projection_case(
        "codex_shim.projection.local_interrupt_preempts_core_state",
    );
    assert_eq!(local_interrupt.request_state, "processing");
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

    let thread_status_cases = lean_codex_shim_thread_status_cases();
    assert_eq!(thread_status_cases.len(), 10);
    for case in thread_status_cases {
        use gents_protocol::client_protocol::ClientTurnState;
        let head = case.request_state.as_deref().and_then(|request_state| {
            gents_protocol::client_protocol::project_persisted_attempt(request_state, false)
        });
        let expected = match head.map(|head| head.turn_state) {
            Some(ClientTurnState::WaitingForClaim) => "active",
            Some(ClientTurnState::Running) => "active",
            Some(ClientTurnState::Failed) => "systemError",
            Some(
                ClientTurnState::Completed
                | ClientTurnState::Superseded
                | ClientTurnState::Interrupted,
            ) => "idle",
            None => "idle",
        };
        assert_eq!(case.projected_status, expected, "{}", case.witness);
    }

    let behavior_cases = lean_codex_shim_behavior_selection_cases();
    assert_eq!(behavior_cases.len(), 5);
    for case in behavior_cases {
        let exact_scope = case.selected_owner == case.actual_owner
            && case.projected_behavior_id == case.actual_behavior;
        if !exact_scope || case.resolved_model.is_none() {
            assert!(
                case.projected_model.is_none(),
                "{}: unavailable or foreign bindings cannot supply display metadata",
                case.witness
            );
        } else {
            assert_eq!(
                case.projected_model, case.resolved_model,
                "{}: exact binding must preserve the selected model",
                case.witness
            );
        }
    }
    assert!(behavior_cases
        .iter()
        .any(|case| case.selected_owner != case.actual_owner));
    assert!(behavior_cases
        .iter()
        .any(|case| case.projected_behavior_id != case.actual_behavior));
    assert!(behavior_cases
        .iter()
        .any(|case| case.resolved_model.is_none()));

    let tool_metadata_cases = lean_codex_shim_tool_metadata_cases();
    assert_eq!(tool_metadata_cases.len(), 11);
    let context_cases = lean_codex_shim_context_usage_cases();
    assert_eq!(context_cases.len(), 2);
    let compaction_cases = lean_codex_shim_compaction_projection_cases();
    assert_eq!(compaction_cases.len(), 8);
}

pub(super) fn generated_codex_shim_binding_cases_pin_runnable_gated_binding() {
    use gents::codex_shim_binding::{ShimBinding, ShimBindingState, ShimUnboundReason};

    let cases = lean_codex_shim_binding_cases();
    assert_eq!(
        cases.len(),
        5,
        "the Lean binding contract must stay fully consumed"
    );

    const BOUND_BEHAVIOR: &str = "default";

    let reason_of = |state: ShimBindingState| match state {
        ShimBindingState::Bound => None,
        ShimBindingState::Unbound(ShimUnboundReason::DependencyMissing) => {
            Some("dependencyMissing")
        }
        ShimBindingState::Unbound(ShimUnboundReason::HostResource) => Some("hostResource"),
    };

    for case in cases {
        assert!(
            !case.requires_restart,
            "{}: convergence must follow from a published generation, never a restart",
            case.witness
        );

        let mut shim = match (case.pre_state.as_str(), case.unbound_reason.as_deref()) {
            ("bound", None) => ShimBinding::bound(BOUND_BEHAVIOR),
            ("unbound", Some("dependencyMissing")) => {
                ShimBinding::unbound(BOUND_BEHAVIOR, ShimUnboundReason::DependencyMissing)
            }
            ("unbound", Some("hostResource")) => {
                ShimBinding::unbound(BOUND_BEHAVIOR, ShimUnboundReason::HostResource)
            }
            other => panic!("{}: unmodeled pre-state {other:?}", case.witness),
        };

        let runnable: Vec<&str> = if case.bound_behavior_runnable {
            vec!["other", BOUND_BEHAVIOR]
        } else {
            vec!["other"]
        };

        let mut listen_attempts = 0usize;
        let host_can_listen = case.host_can_listen;
        let state = shim.observe_publish(runnable.iter().copied(), || {
            listen_attempts += 1;
            host_can_listen
        });

        let observed = if shim.is_bound() { "bound" } else { "unbound" };
        assert_eq!(
            observed, case.post_state,
            "{}: observing a published generation must land in the modeled state",
            case.witness
        );
        assert_eq!(
            reason_of(state),
            case.post_unbound_reason.as_deref(),
            "{}: the unbound class decides whether a later generation may revive \
             the shim; it must match the model",
            case.witness
        );

        let expected_attempts = usize::from(
            case.bound_behavior_runnable
                && case.pre_state == "unbound"
                && case.unbound_reason.as_deref() == Some("dependencyMissing"),
        );
        assert_eq!(
            listen_attempts, expected_attempts,
            "{}: the host must attempt the listen exactly when the generation grants it",
            case.witness
        );

        // Re-observing the same generation must change nothing
        // (CodexShim.Binding.Shim.observePublish_idempotent).
        let settled = shim.clone();
        shim.observe_publish(runnable.iter().copied(), || host_can_listen);
        assert_eq!(
            shim, settled,
            "{}: re-observing one generation must be idempotent",
            case.witness
        );
    }
}
