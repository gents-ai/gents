//! Runtime backgrounding model metadata and shared vocabulary checks.

use super::*;

pub(super) async fn generated_r6_backgrounding_case_metadata_matches_export() {
    let cases = lean_r6_backgrounding_cases();
    assert_eq!(cases.len(), 42);

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "no_goal_preserves_background_wake",
            "active_goal_owns_background_continuation",
            "paused_goal_does_not_background_resume",
            "blocked_goal_does_not_background_resume",
            "usage_limited_goal_does_not_background_resume",
            "budget_limited_goal_owns_wrapup",
            "complete_goal_does_not_background_resume",
            "background_tool_budget_count_7_admits_spawn",
            "background_tool_budget_count_8_rejects_spawn",
            "tool_kind_background_mode_executes",
            "tool_kind_bridge_complete_persists_result",
            "tool_kind_explicit_cancel_projects_explicit_cancel",
            "background_recovery_running_live_parent_to_cancelled",
            "background_completion_source_writes_canonical_key",
            "terminal_completion_message_precedes_claimed_continuation",
            "failed_background_wake_with_budget_redrives",
            "failed_background_wake_exhausted_budget_stops",
            "generic_scheduled_failure_is_not_background_redrive",
            "non_latest_background_wake_does_not_redrive",
            "aged_background_wake_precedes_new_descendant",
            "fresh_background_wake_preserves_fifo",
            "completed_wake_acknowledges_exact_claim_snapshot",
            "failed_wake_retains_claim_snapshot_unacknowledged",
            "restart_before_claim_preserves_pending_notification",
            "live_inference_retains_snapshot_without_ack_or_redrive",
            "acknowledgement_projection_restart_is_atomic",
            "noncanonical_subagent_completion_source_is_rejected",
            "list_processes_same_requester_next_turn_authorized",
            "read_process_same_requester_next_turn_authorized",
            "wait_process_same_requester_next_turn_authorized",
            "cancel_process_same_requester_next_turn_authorized",
            "originating_request_without_matching_requester_is_denied",
            "absent_requester_next_turn_authorized",
            "empty_requester_does_not_alias_absent",
            "process_control_cross_session_denied",
            "process_control_cross_agent_denied",
            "process_control_cross_requester_denied",
            "wait_timeout_preserves_running_process",
            "caller_deadline_times_out_wait_call_preserves_background_process",
            "caller_interrupt_cancels_wait_call_preserves_background_process",
            "caller_interrupt_preserves_running_process",
            "caller_deadline_preserves_running_process",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.max_backgrounded, 8, "{}", case.name);
        assert!(
            case.pre_live_count <= case.max_backgrounded,
            "{}",
            case.name
        );
        // This export also contains foreground wait callers and child-linked
        // cases. Their mode, policy and linkage are modeled inputs consumed by
        // the relevant native owner tests, not uniform background defaults.
    }

    let admit = lean_r6_backgrounding_case("background_tool_budget_count_7_admits_spawn");
    assert!(admit.legal);
    assert_eq!(admit.pre_live_count, 7);
    assert_eq!(admit.terminal_state.as_str(), "running");

    let reject = lean_r6_backgrounding_case("background_tool_budget_count_8_rejects_spawn");
    assert!(!reject.legal);
    assert_eq!(reject.pre_live_count, 8);
    assert_eq!(
        reject.error_code.as_deref(),
        Some("background_tool_budget_exceeded")
    );

    let completed = lean_r6_backgrounding_case("tool_kind_bridge_complete_persists_result");
    assert!(completed.legal);
    assert_eq!(completed.terminal_state.as_str(), "completed");
    assert_eq!(completed.result.as_deref(), Some("done"));

    let cancelled =
        lean_r6_backgrounding_case("tool_kind_explicit_cancel_projects_explicit_cancel");
    assert_eq!(cancelled.terminal_state.as_str(), "cancelled");
    assert_eq!(cancelled.reason.as_deref(), Some("explicit_cancel"));

    let backgrounded = lean_r6_backgrounding_case("tool_kind_background_mode_executes");
    assert!(backgrounded.legal);
    assert_eq!(backgrounded.action.as_str(), "background");
    assert_eq!(backgrounded.terminal_state.as_str(), "running");

    let recovered =
        lean_r6_backgrounding_case("background_recovery_running_live_parent_to_cancelled");
    assert_eq!(
        recovered.action.as_str(),
        "TerminalizeBackgroundedAsInterrupted"
    );
    assert_eq!(recovered.terminal_state.as_str(), "cancelled");
    assert_eq!(recovered.reason.as_deref(), Some("interrupted_on_restart"));
    assert_eq!(
        recovered.queue_source.as_deref(),
        Some("background_completion")
    );
    assert_eq!(
        recovered.queue_key.as_deref(),
        Some("background_completion:900")
    );

    let canonical = lean_r6_backgrounding_case("background_completion_source_writes_canonical_key");
    assert_eq!(
        canonical.queue_source.as_deref(),
        Some("background_completion")
    );
    assert_eq!(
        canonical.queue_key.as_deref(),
        Some("background_completion:900")
    );

    let noncanonical =
        lean_r6_backgrounding_case("noncanonical_subagent_completion_source_is_rejected");
    assert_eq!(
        noncanonical.queue_source.as_deref(),
        Some("subagent_completion")
    );
    assert_eq!(noncanonical.queue_key, None);

    let redrive = lean_r6_backgrounding_case("failed_background_wake_with_budget_redrives");
    assert!(redrive.legal);
    assert_eq!(redrive.group, "completion_redrive");
    assert_eq!(redrive.action, "redrive_failed_background_wake");
    assert_eq!(redrive.retry_count, Some(1));
    assert_eq!(redrive.max_retries, Some(3));
    assert_eq!(redrive.post_retry_count, Some(2));
    assert_eq!(redrive.retry_delay_seconds, Some(10));
    assert_eq!(
        gents::lifecycle::background_wake_retry_delay(redrive.retry_count.unwrap() as i64)
            .num_seconds(),
        redrive.retry_delay_seconds.unwrap() as i64
    );
    assert_eq!(redrive.is_latest, Some(true));

    let exhausted = lean_r6_backgrounding_case("failed_background_wake_exhausted_budget_stops");
    assert!(!exhausted.legal);
    assert_eq!(exhausted.retry_count, exhausted.max_retries);
    assert_eq!(exhausted.post_retry_count, None);

    let generic = lean_r6_backgrounding_case("generic_scheduled_failure_is_not_background_redrive");
    assert!(!generic.legal);
    assert_eq!(generic.queue_source.as_deref(), Some("user"));

    let non_latest = lean_r6_backgrounding_case("non_latest_background_wake_does_not_redrive");
    assert!(!non_latest.legal);
    assert_eq!(non_latest.is_latest, Some(false));

    let aged = lean_r6_backgrounding_case("aged_background_wake_precedes_new_descendant");
    assert!(aged.legal);
    assert_eq!(aged.group, "completion_admission");
    assert_eq!(aged.action, "rank_pending_background_wake");
    assert_eq!(aged.reason.as_deref(), Some("aged_priority"));

    let fresh = lean_r6_backgrounding_case("fresh_background_wake_preserves_fifo");
    assert!(!fresh.legal);
    assert_eq!(fresh.group, "completion_admission");
    assert_eq!(fresh.reason.as_deref(), Some("fifo"));

    let acknowledged =
        lean_r6_backgrounding_case("completed_wake_acknowledges_exact_claim_snapshot");
    assert!(acknowledged.legal);
    assert_eq!(acknowledged.group, "completion_acknowledgement");
    assert_eq!(acknowledged.terminal_state, "completed");
    assert_eq!(
        acknowledged.result.as_deref(),
        Some("attempted=1,acknowledged=1")
    );
    assert_eq!(acknowledged.reason.as_deref(), Some("completed_ack"));

    let retained = lean_r6_backgrounding_case("failed_wake_retains_claim_snapshot_unacknowledged");
    assert!(retained.legal);
    assert_eq!(retained.group, "completion_acknowledgement");
    assert_eq!(retained.terminal_state, "failed");
    assert_eq!(
        retained.result.as_deref(),
        Some("attempted=1,acknowledged=0")
    );
    assert_eq!(retained.reason.as_deref(), Some("failed_unacknowledged"));

    let before_claim =
        lean_r6_backgrounding_case("restart_before_claim_preserves_pending_notification");
    assert!(before_claim.legal);
    assert_eq!(before_claim.group, "completion_failure_boundary");
    assert_eq!(before_claim.action, "restart_before_claim");
    assert_eq!(before_claim.terminal_state, "pending");
    assert_eq!(
        before_claim.result.as_deref(),
        Some("attempted=0,acknowledged=0")
    );
    assert_eq!(before_claim.reason.as_deref(), Some("pending_reclaim"));

    let during_inference =
        lean_r6_backgrounding_case("live_inference_retains_snapshot_without_ack_or_redrive");
    assert!(during_inference.legal);
    assert_eq!(during_inference.action, "preserve_live_inference");
    assert_eq!(during_inference.terminal_state, "processing");
    assert_eq!(
        during_inference.result.as_deref(),
        Some("attempted=1,acknowledged=0")
    );
    assert_eq!(during_inference.reason.as_deref(), Some("owner_still_live"));

    let during_ack = lean_r6_backgrounding_case("acknowledgement_projection_restart_is_atomic");
    assert!(during_ack.legal);
    assert_eq!(during_ack.action, "project_acknowledgement_after_restart");
    assert_eq!(during_ack.terminal_state, "completed");
    assert_eq!(
        during_ack.result.as_deref(),
        Some("attempted=1,acknowledged=1")
    );
    assert_eq!(during_ack.reason.as_deref(), Some("atomic_ack_projection"));

    // Completion-owner cases bind accepted native publication in the private
    // lifecycle owner test, registered in conformance_consumers.rs.

    // The continuation ordering/claim witness is bound through canonical
    // publication in completion_owner_conformance, alongside the owner cases.

    for action in [
        "list_processes",
        "read_process",
        "wait_process",
        "cancel_process",
    ] {
        let case = cases
            .iter()
            .find(|case| {
                case.group == "process_control_authorization"
                    && case.action == action
                    && case.reason.as_deref() == Some("same_requester_next_turn")
            })
            .unwrap_or_else(|| panic!("missing same-principal process control case for {action}"));
        assert!(case.legal, "{} must remain authorized", case.name);
    }

    for scenario in ["cross_session", "cross_agent", "cross_requester"] {
        let case = cases
            .iter()
            .find(|case| {
                case.group == "process_control_authorization"
                    && case.reason.as_deref() == Some(scenario)
            })
            .unwrap_or_else(|| panic!("missing denied process control case for {scenario}"));
        assert!(!case.legal, "{} must be denied", case.name);
    }

    assert!(lean_r6_backgrounding_case("absent_requester_next_turn_authorized").legal);
    assert!(!lean_r6_backgrounding_case("empty_requester_does_not_alias_absent").legal);

    for reason in [
        "wait_timeout",
        "caller_interrupted",
        "caller_deadline_exceeded",
    ] {
        let case = cases
            .iter()
            .find(|case| case.group == "wait_boundary" && case.reason.as_deref() == Some(reason))
            .unwrap_or_else(|| panic!("missing wait boundary case for {reason}"));
        assert!(case.legal, "{} must not request cancellation", case.name);
        assert_eq!(case.terminal_state, "running", "{}", case.name);
    }
    let dispatch_deadline = lean_r6_backgrounding_case(
        "caller_deadline_times_out_wait_call_preserves_background_process",
    );
    assert_eq!(dispatch_deadline.group, "wait_dispatch_boundary");
    assert!(dispatch_deadline.legal);
    assert_eq!(dispatch_deadline.action, "wait_process");
    assert_eq!(dispatch_deadline.terminal_state, "timedOut");
    assert_eq!(dispatch_deadline.result.as_deref(), Some("running"));
    assert_eq!(
        dispatch_deadline.reason.as_deref(),
        Some("caller_deadline_exceeded")
    );
}

pub(super) fn delegation_depth_matches_runtime_limit() {
    let cases = lean_subagent_delegation_graph_cases();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(
            case.max_depth,
            usize::try_from(MAX_SUBAGENT_DEPTH).expect("MAX_SUBAGENT_DEPTH fits usize"),
            "{}: Lean and runtime delegation depth limits differ",
            case.name,
        );
    }
}

pub(super) fn unmaterialized_child_status_matches_runtime_vocabulary() {
    let LeanR4cBackgroundWorkCase::UnmaterializedChildVisible {
        listed_status,
        read_lifecycle_state,
        ..
    } = lean_r4c_background_work_case("r4c.list_subagents.unmaterialized_child_visible")
    else {
        panic!("unmaterialized child witness variant drifted");
    };
    for status in [listed_status, read_lifecycle_state] {
        assert_eq!(
            status,
            gents::__test_internals::AWAITING_CHILD_MATERIALIZATION
        );
    }
}

// The retired volatile-registry dispatch witness is intentionally not a native
// consumer of canonical_source_reconstruction. Its replacement must export
// modeled segment inputs and exercise durable open/closed reads, missing/twin
// rejection, and late-suffix stability; CoverageLedger tracks that binding.
