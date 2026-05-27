use super::*;

pub(super) fn generated_codex_shim_projection_cases_pin_adapter_mapping() {
    let cases = lean_codex_shim_projection_cases();
    assert_eq!(cases.len(), 11);

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
    assert_eq!(
        pending.lean_theorems,
        vec![
            "CodexShim.project_pending_is_in_progress".to_string(),
            "CodexShim.nonterminal_without_response_projects_in_progress".to_string(),
            "CodexShim.request_transition_projection_monotonic".to_string(),
        ]
    );

    let completed = lean_codex_shim_projection_case("codex_shim.projection.completed_request");
    assert_eq!(completed.request_state, "completed");
    assert_eq!(completed.response_status.as_deref(), Some("error"));
    assert!(!completed.local_interrupt_acked);
    assert_eq!(completed.projected_phase, "completed");
    assert!(completed.terminal);
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
    assert_eq!(
        local_interrupt.lean_theorems,
        vec![
            "CodexShim.local_interrupt_projects_interrupted".to_string(),
            "CodexShim.local_interrupt_never_projects_in_progress".to_string(),
        ]
    );
}

pub(super) fn generated_codex_shim_steering_cases_pin_adapter_contract() {
    let cases = lean_codex_shim_steering_cases();
    assert_eq!(cases.len(), 3);

    let names = cases
        .iter()
        .map(|case| case.witness.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "codex_shim.turn_steer.accepted_same_turn",
            "codex_shim.turn_steer.drain_queued_request",
            "codex_shim.turn_interrupt.local_terminal",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    let accepted = lean_codex_shim_steering_case("codex_shim.turn_steer.accepted_same_turn");
    assert_eq!(accepted.active_turn_id, "turn-active");
    assert_eq!(accepted.expected_turn_id, accepted.active_turn_id);
    assert_eq!(accepted.active_request_id, "request-active");
    assert!(!accepted.emits_turn_started);
    assert!(!accepted.emits_turn_completed);
    assert!(accepted.preserves_active_turn);
    assert!(!accepted.clears_active_turn);
    assert_eq!(accepted.terminal_status, None);
    assert_eq!(accepted.committed_user_message_delta, 1);
    assert_eq!(accepted.queue_source.as_deref(), Some("steering"));
    assert_eq!(accepted.queue_policy.as_deref(), Some("append"));
    assert_eq!(
        accepted.queued_after_request_id.as_deref(),
        Some(accepted.active_request_id.as_str())
    );
    assert!(!accepted.forwards_request_interrupt);
    assert!(!accepted.requires_request_transition_before_ack);
    assert_eq!(
        accepted.lean_theorems,
        vec![
            "CodexShim.accept_steer_preserves_active_turn".to_string(),
            "CodexShim.accept_steer_does_not_emit_turn_started".to_string(),
            "CodexShim.accept_steer_appends_steering_entry".to_string(),
            "CodexShim.accept_steer_records_queued_request".to_string(),
        ]
    );

    let drain = lean_codex_shim_steering_case("codex_shim.turn_steer.drain_queued_request");
    assert_eq!(drain.active_turn_id, "turn-active");
    assert!(!drain.emits_turn_started);
    assert!(!drain.emits_turn_completed);
    assert!(drain.preserves_active_turn);
    assert!(!drain.clears_active_turn);
    assert_eq!(drain.committed_user_message_delta, 0);
    assert_eq!(drain.queue_source, None);
    assert_eq!(drain.queue_policy, None);
    assert!(!drain.forwards_request_interrupt);
    assert!(!drain.requires_request_transition_before_ack);
    assert_eq!(drain.request_transition.as_deref(), Some("drain_steering"));
    assert_eq!(drain.request_from.as_deref(), Some("projectedCompleted"));
    assert_eq!(drain.request_to.as_deref(), Some("inProgress"));
    assert_eq!(
        drain.lean_theorems,
        vec![
            "CodexShim.drain_steering_advances_active_request_without_completing_turn".to_string(),
            "CodexShim.drain_steering_uses_completed_projection".to_string(),
        ]
    );

    let interrupted = lean_codex_shim_steering_case("codex_shim.turn_interrupt.local_terminal");
    assert_eq!(interrupted.active_turn_id, "turn-active");
    assert_eq!(interrupted.expected_turn_id, interrupted.active_turn_id);
    assert!(!interrupted.emits_turn_started);
    assert!(interrupted.emits_turn_completed);
    assert!(!interrupted.preserves_active_turn);
    assert!(interrupted.clears_active_turn);
    assert_eq!(interrupted.terminal_status.as_deref(), Some("interrupted"));
    assert_eq!(interrupted.committed_user_message_delta, 0);
    assert_eq!(interrupted.queue_source, None);
    assert_eq!(interrupted.queue_policy, None);
    assert!(interrupted.forwards_request_interrupt);
    assert!(!interrupted.requires_request_transition_before_ack);
    assert_eq!(interrupted.request_transition, None);
    assert_eq!(interrupted.request_from, None);
    assert_eq!(interrupted.request_to, None);
    assert_eq!(
        interrupted.lean_theorems,
        vec![
            "CodexShim.interrupt_active_clears_active_turn".to_string(),
            "CodexShim.interrupt_active_emits_terminal_turn".to_string(),
            "CodexShim.interrupt_active_does_not_wait_for_request_transition".to_string(),
            "CodexShim.interrupt_active_does_not_preserve_active_turn".to_string(),
            "CodexShim.interrupt_cannot_stutter".to_string(),
        ]
    );
}
