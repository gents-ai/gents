use super::*;
use crate::lean_vocab_test::LeanRestartRowKind;
use gents::tool_call_lifecycle::{AwaitMode, ToolRowKind};

pub(super) async fn generated_r6_backgrounding_case_metadata_matches_export() {
    let cases = lean_r6_backgrounding_cases();
    assert_eq!(cases.len(), 56);

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "no_goal_preserves_background_wake",
            "active_goal_keeps_background_wake",
            "paused_goal_keeps_background_wake",
            "blocked_goal_keeps_background_wake",
            "usage_limited_goal_keeps_background_wake",
            "budget_limited_goal_keeps_background_wake",
            "complete_goal_keeps_background_wake",
            "background_tool_budget_count_7_admits_spawn",
            "background_tool_budget_count_8_rejects_spawn",
            "background_mixed_batch_attempt_0",
            "background_mixed_batch_attempt_1",
            "background_mixed_batch_attempt_2",
            "background_mixed_batch_attempt_3",
            "background_mixed_batch_attempt_4",
            "background_mixed_batch_attempt_5",
            "background_mixed_batch_attempt_6",
            "background_mixed_batch_attempt_7",
            "background_mixed_batch_attempt_8",
            "background_mixed_batch_attempt_9",
            "tool_kind_background_mode_executes",
            "tool_kind_bridge_complete_persists_result",
            "tool_kind_explicit_cancel_projects_explicit_cancel",
            "native_background_row_foregrounds",
            "session_message_row_refuses_foreground",
            "plugin_job_row_refuses_foreground",
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
            "noncanonical_completion_source_is_rejected",
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
            "caller_interrupt_observer_completes_wait_call_preserves_background_process",
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

    let batch = cases
        .iter()
        .filter(|case| case.group == "budget_batch")
        .collect::<Vec<_>>();
    assert_eq!(batch.len(), 10);
    assert_eq!(batch.iter().filter(|case| case.legal).count(), 8);
    assert_eq!(batch.iter().filter(|case| !case.legal).count(), 2);
    assert_eq!(
        batch.iter().map(|case| case.pre_live_count).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 8]
    );
    assert_eq!(
        batch.iter().map(|case| case.error_code.as_deref()).collect::<Vec<_>>(),
        [None, None, None, None, None, None, None, None,
            Some("background_tool_budget_exceeded"),
            Some("background_tool_budget_exceeded")]
    );
    assert_eq!(
        batch.iter().map(|case| case.action.as_str()).collect::<Vec<_>>(),
        ["agent_new", "spawn_process", "agent_message", "plugin_job", "agent_new",
            "spawn_process", "agent_message", "plugin_job", "agent_new", "spawn_process"]
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

    let noncanonical = lean_r6_backgrounding_case("noncanonical_completion_source_is_rejected");
    assert_eq!(
        noncanonical.queue_source.as_deref(),
        Some("unrecognized_completion_source")
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
            .unwrap_or_else(|| panic!("missing same-requester process control case for {action}"));
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

/// Lean `restartDisposition` leaves a plugin-job row running and
/// `ToolCallContext.Transition.foreground` refuses it, through the decision
/// cores of `ToolRowKind` that sit beside the orphan sweep's
/// `is_background_tool_row` and `ToolCallLifecycle::foreground`, which call
/// them: a plugin-job row is outside the orphan sweep's scope and is never
/// foregrounded. Each row supplies its own kind and await mode.
#[tokio::test]
async fn generated_plugin_job_restart_and_foreground_rows_match_owners() {
    let restarts = lean_restart_disposition_cases()
        .iter()
        .filter(|case| case.kind == LeanRestartRowKind::PluginJob)
        .collect::<Vec<_>>();
    assert_eq!(
        restarts.len(),
        3,
        "Lean should emit the plugin-job restart rows"
    );
    for case in restarts {
        let name = case.name.as_str();
        assert!(!case.session_message, "{name}");
        assert_eq!(case.disposition, "leave_running", "{name}");
        assert_eq!(case.cause, None, "{name}");
        assert_eq!(case.terminal_state, None, "{name}");
        assert_eq!(case.notification_reason, None, "{name}");
        assert_eq!(case.queue_source, None, "{name}");
        let await_mode = AwaitMode::from_persisted(&case.await_mode)
            .unwrap_or_else(|| panic!("{name}: unknown await mode {:?}", case.await_mode));
        assert!(
            !ToolRowKind::PluginJob.in_orphan_sweep_scope(await_mode),
            "{name}: the orphan sweep must never select a plugin-job row"
        );
    }

    let refusal = lean_r6_backgrounding_case("plugin_job_row_refuses_foreground");
    assert_eq!(refusal.group, "native_lifecycle");
    assert_eq!(refusal.action, "foreground");
    assert_eq!(refusal.operation.as_deref(), Some("pluginJob"));
    assert_eq!(refusal.await_mode, "background");
    assert_eq!(refusal.terminal_state, "rejected");
    assert!(!refusal.legal);
    let kind = match refusal.operation.as_deref() {
        Some("pluginJob") => ToolRowKind::PluginJob,
        other => panic!("plugin_job_row_refuses_foreground: unexpected operation {other:?}"),
    };
    assert_eq!(
        kind.may_foreground(),
        refusal.legal,
        "a plugin-job row must refuse the foreground flip"
    );
}
