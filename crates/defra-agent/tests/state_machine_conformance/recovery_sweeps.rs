use super::*;

pub(super) fn generated_recovery_sweep_cases_pin_startup_recovery_contract() {
    let cases = lean_recovery_sweep_cases();
    assert_eq!(
        cases.len(),
        19,
        "Lean should emit one row per registered recovery predicate witness"
    );

    let expected_sweep_ids = [
        "request_lifecycle_recover_all_requests",
        "request_lifecycle_recover_all_streaming_responses",
        "tool_call_lifecycle_recover_all_running_calls",
        "tool_call_lifecycle_recover_detached_bridge_rows",
        "inference_call_recover_all_stale_calls",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual_sweep_ids = cases
        .iter()
        .map(|case| case.sweep_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_sweep_ids, expected_sweep_ids,
        "Lean recovery sweep registry drifted"
    );

    for case in cases {
        assert_eq!(case.cadence.as_str(), "startup");
        assert!(
            case.measure_before > case.measure_after,
            "recovery case {} must decrease its measure",
            case.name
        );
        assert_eq!(
            case.measure_after, 0,
            "recovery case {} must reach zero measure",
            case.name
        );
        assert_ne!(
            case.terminal_state.as_str(),
            "running",
            "recovery case {} must not leave a stale row running",
            case.name
        );
        assert!(
            !case.deadline_audit_ref.trim().is_empty(),
            "recovery case {} must name its audit reference",
            case.name
        );
    }

    let implemented = [
        "request_lifecycle_recover_all_requests",
        "request_lifecycle_recover_all_streaming_responses",
        "tool_call_lifecycle_recover_all_running_calls",
    ];
    for sweep_id in implemented {
        for case in cases.iter().filter(|case| case.sweep_id == sweep_id) {
            assert_eq!(
                case.implementation_status.as_str(),
                "implemented",
                "sweep {sweep_id} should be an implemented startup sweep"
            );
        }
    }

    let detached_cases = cases
        .iter()
        .filter(|case| case.sweep_id == "tool_call_lifecycle_recover_detached_bridge_rows")
        .collect::<Vec<_>>();
    assert_eq!(
        detached_cases.len(),
        5,
        "detached bridge recovery must have explicit obligation witnesses"
    );
    for case in detached_cases {
        assert_eq!(case.collection.as_str(), "AgentToolCall");
        assert_eq!(
            case.rust_function.as_str(),
            "ToolCallLifecycle::recover_detached_bridge_rows"
        );
        assert_eq!(case.implementation_status.as_str(), "obligation");
        assert!(
            case.deadline_audit_ref
                .contains("subagent-bridge-terminal-lifetime"),
            "detached bridge case {} must point at the bridge terminal lifetime gap",
            case.name
        );
        assert!(
            ["completed", "failed", "cancelled", "timedOut"]
                .contains(&case.terminal_state.as_str()),
            "detached bridge case {} must terminalize, not skip",
            case.name
        );
    }

    let queued = lean_recovery_sweep_case("inference_queued_stale_to_cancelled");
    assert_eq!(queued.pre_state.as_str(), "queued");
    assert_eq!(queued.terminal_state.as_str(), "cancelled");

    let running = lean_recovery_sweep_case("inference_running_stale_to_failed");
    assert_eq!(running.pre_state.as_str(), "running");
    assert_eq!(running.terminal_state.as_str(), "failed");

    let interrupted = lean_recovery_sweep_case("inference_interrupted_parent_to_cancelled");
    assert_eq!(interrupted.terminal_state.as_str(), "cancelled");

    for case in cases
        .iter()
        .filter(|case| case.sweep_id == "inference_call_recover_all_stale_calls")
    {
        assert_eq!(case.collection.as_str(), "InferenceCall");
        assert_eq!(case.rust_function.as_str(), "InferenceCall::recover_all");
        assert_eq!(case.implementation_status.as_str(), "obligation");
        assert!(
            case.deadline_audit_ref.contains("follow-up-6-pr-e"),
            "InferenceCall recovery case {} must point at deadline audit PR E",
            case.name
        );
        let terminal_row =
            InferenceCallSlotRow::new("contract-backend", case.terminal_state.as_str());
        assert_eq!(
            slot_contribution(terminal_row, "contract-backend"),
            0,
            "terminal InferenceCall recovery case {} must release its backend slot",
            case.name
        );
        assert_eq!(
            reconstructed_running_slot_count([terminal_row], "contract-backend"),
            0,
            "terminal InferenceCall recovery case {} must reconstruct zero running slots",
            case.name
        );
    }
}
