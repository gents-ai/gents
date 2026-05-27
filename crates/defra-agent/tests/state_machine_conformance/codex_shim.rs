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
