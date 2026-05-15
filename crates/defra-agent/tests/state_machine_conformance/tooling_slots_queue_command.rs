use super::*;

pub(super) fn generated_tool_execution_cases_cover_preflight_and_retry_contracts() {
    let unreachable =
        lean_tool_preflight_case("preflight_unreachable_valid_blocks_serviceUnavailable");
    assert_eq!(unreachable.decision, "block");
    assert_eq!(
        unreachable.failure_class.as_deref(),
        Some("serviceUnavailable")
    );

    let invalid = lean_tool_preflight_case("preflight_healthy_invalid_blocks_argumentInvalid");
    assert_eq!(invalid.decision, "block");
    assert_eq!(invalid.failure_class.as_deref(), Some("argumentInvalid"));

    for name in [
        "preflight_healthy_valid_dispatch",
        "preflight_stale_valid_dispatch",
    ] {
        let case = lean_tool_preflight_case(name);
        assert_eq!(case.decision, "dispatch", "{name}");
        assert_eq!(case.failure_class, None, "{name}");
    }

    let safe_read = lean_tool_retry_case("retry_mcpListTools_unknown_transport_retrySafeRead");
    assert_eq!(safe_read.disposition, "retrySafeRead");

    let idempotent =
        lean_tool_retry_case("retry_mcpCall_idempotent_transport_retryIdempotentToolCall");
    assert_eq!(idempotent.disposition, "retryIdempotentToolCall");

    for name in [
        "retry_mcpCall_unknown_transport_doNotRetry",
        "retry_mcpCall_nonIdempotent_transport_doNotRetry",
        "retry_nativeCommand_idempotent_transport_doNotRetry",
    ] {
        let case = lean_tool_retry_case(name);
        assert_eq!(case.disposition, "doNotRetry", "{name}");
    }
}

fn slot_rows_from_contract<'a>(
    backend_ids: &'a [String],
    row_states: &'a [String],
) -> impl Iterator<Item = InferenceCallSlotRow<'a>> {
    backend_ids
        .iter()
        .zip(row_states)
        .map(|(backend_id, state)| InferenceCallSlotRow::new(backend_id.as_str(), state.as_str()))
}

pub(super) fn generated_slot_accounting_cases_pin_inference_and_fleet_contracts() {
    for case in &lean_contract_snapshot().inference_slot_accounting_cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Inference slot case {} emitted mismatched row arrays",
            case.name
        );
        if case.row_states.len() == 1 {
            let row = InferenceCallSlotRow::new(
                case.row_backend_ids[0].as_str(),
                case.row_states[0].as_str(),
            );
            assert_eq!(
                slot_contribution(row, &case.backend_id),
                case.expected_contribution,
                "Inference slot case {} drifted from Rust slot contribution",
                case.name
            );
        }
        let reconstructed = reconstructed_running_slot_count(
            slot_rows_from_contract(&case.row_backend_ids, &case.row_states),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "Inference slot case {} drifted from Rust admission reconstruction",
            case.name
        );
    }

    let queued = lean_inference_slot_accounting_case("queued_contributes_zero");
    assert_eq!(queued.property.as_str(), "state_contribution");
    assert_eq!(queued.pre_state.as_str(), "queued");
    assert_eq!(queued.contribution, 0);
    assert_eq!(queued.reconstructed_running_count, 0);

    let running = lean_inference_slot_accounting_case("running_contributes_one");
    assert_eq!(running.pre_state.as_str(), "running");
    assert_eq!(running.contribution, 1);
    assert_eq!(running.expected_contribution, 1);

    for name in [
        "cancelled_terminal_contributes_zero",
        "completed_terminal_contributes_zero",
        "failed_terminal_contributes_zero",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(case.property.as_str(), "state_contribution");
        assert_eq!(case.contribution, 0, "{name}");
        assert_eq!(case.reconstructed_running_count, 0, "{name}");
    }

    for name in [
        "cancelled_releases_slot",
        "completed_releases_slot",
        "failed_releases_slot",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(case.property.as_str(), "terminal_release", "{name}");
        assert_eq!(case.pre_state.as_str(), "running", "{name}");
        assert_eq!(case.pre_contribution, 1, "{name}");
        assert_eq!(case.post_contribution, 0, "{name}");
        assert!(case.released_slot, "{name}");
    }

    for name in [
        "permit_drop_failed_terminalization_not_counted",
        "permit_drop_cancelled_terminalization_not_counted",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(
            case.property.as_str(),
            "permit_drop_terminalization",
            "{name}"
        );
        assert!(case.permit_drop_terminalization, "{name}");
        assert_eq!(case.post_contribution, 0, "{name}");
    }

    let bounded = lean_inference_slot_accounting_case(
        "reconstructed_running_count_bounded_by_max_concurrent",
    );
    assert_eq!(bounded.reconstructed_running_count, 1);
    assert_eq!(bounded.max_concurrent, 1);
    assert!(bounded.bounded_by_max_concurrent);
    assert_eq!(
        bounded.row_states,
        vec![
            "running".to_string(),
            "queued".to_string(),
            "completed".to_string(),
            "running".to_string()
        ]
    );

    let fleet_ledger = lean_contract_snapshot()
        .coverage_ledger
        .iter()
        .find(|entry| entry.category == "fleet_cases" && entry.domain == "FleetSlotAccounting")
        .expect("FleetSlotAccounting coverage ledger entry must be emitted");
    assert_eq!(
        fleet_ledger.accepted_boundary.as_str(),
        "boundary.fleet-slot-accounting.derived-view",
        "FleetSlotAccounting must be classified as a derived boundary, not a persisted aggregate"
    );

    for case in &lean_contract_snapshot().fleet_slot_accounting_cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Fleet slot case {} emitted mismatched projection row arrays",
            case.name
        );
        if case.row_states.len() == 1 {
            if case.admission_state == "released" {
                let expected_terminal_state = match case.request_state.as_str() {
                    "completed" => "completed",
                    "failed" => "failed",
                    "interrupted" | "superseded" | "dead" => "cancelled",
                    other => panic!(
                        "Fleet slot released case {} has non-terminal request_state={other}",
                        case.name
                    ),
                };
                assert_eq!(
                    case.row_states[0].as_str(),
                    expected_terminal_state,
                    "Fleet slot released case {} projected the wrong terminal InferenceCall state",
                    case.name
                );
            }
            let row = InferenceCallSlotRow::new(
                case.row_backend_ids[0].as_str(),
                case.row_states[0].as_str(),
            );
            assert_eq!(
                slot_contribution(row, &case.backend_id),
                case.expected_contribution,
                "Fleet slot case {} drifted from Rust slot contribution",
                case.name
            );
        }
        let reconstructed = reconstructed_running_slot_count(
            slot_rows_from_contract(&case.row_backend_ids, &case.row_states),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "Fleet slot case {} drifted from Rust admission reconstruction",
            case.name
        );
        assert_eq!(
            reconstructed, case.slot_count,
            "Fleet slot case {} must be a derived projection over admission reconstruction",
            case.name
        );
        assert_eq!(
            case.scheduler_running, case.slot_count,
            "Fleet slot case {} must keep aggregate running reconstructed from slot count",
            case.name
        );
        assert_eq!(
            case.contribution, case.expected_contribution,
            "Fleet slot case {} must compute its expected contribution",
            case.name
        );
        assert_eq!(
            case.bounded_by_max_concurrent,
            case.slot_count <= case.max_concurrent,
            "Fleet slot case {} must compute its max_concurrent bound",
            case.name
        );
        assert!(
            case.aggregate_reconstructed_not_persisted,
            "Fleet slot case {} must preserve reconstructed-not-persisted policy",
            case.name
        );
    }

    let waiting = lean_fleet_slot_accounting_case("fleet_waiting_contributes_zero");
    assert_eq!(waiting.admission_state.as_str(), "waiting");
    assert_eq!(waiting.contribution, 0);

    let acquired = lean_fleet_slot_accounting_case("fleet_acquired_contributes_one");
    assert_eq!(acquired.admission_state.as_str(), "acquired");
    assert_eq!(acquired.contribution, 1);

    let executing = lean_fleet_slot_accounting_case("fleet_executing_contributes_one");
    assert_eq!(executing.admission_state.as_str(), "executing");
    assert_eq!(executing.contribution, 1);

    let released = lean_fleet_slot_accounting_case("fleet_released_terminal_contributes_zero");
    assert_eq!(released.request_state.as_str(), "completed");
    assert_eq!(released.admission_state.as_str(), "released");
    assert_eq!(released.contribution, 0);

    let fleet_bound = lean_fleet_slot_accounting_case(
        "fleet_reconstructed_running_count_bounded_by_max_concurrent",
    );
    assert_eq!(fleet_bound.slot_count, fleet_bound.scheduler_running);
    assert_eq!(fleet_bound.slot_count, 2);
    assert_eq!(fleet_bound.reconstructed_running_count, 2);
    assert_eq!(
        fleet_bound.row_states,
        vec![
            "running".to_string(),
            "running".to_string(),
            "queued".to_string(),
            "completed".to_string()
        ]
    );
    assert_eq!(fleet_bound.max_concurrent, 2);
    assert!(fleet_bound.bounded_by_max_concurrent);
}

pub(super) fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    let cases = lean_queue_deadline_cases();
    assert_eq!(cases.len(), 5);

    let emitted_names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        emitted_names,
        [
            "active_request_blocks_later_same_session_claim",
            "terminal_active_allows_next_pending_same_session_claim",
            "background_completion_session_coalesces_one_pending_wakeup",
            "cancel_drains_automated_wakeups_preserves_user_pending",
            "claim_preserves_explicit_deadline",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.session_id, 900, "{}", case.name);
        assert!(
            case.superseded_request_ids.is_empty(),
            "{} must be a queue/deadline contract, not a supersession contract",
            case.name
        );
    }

    let blocked = lean_queue_deadline_case("active_request_blocks_later_same_session_claim");
    assert_eq!(blocked.group, "queue_admission");
    assert_eq!(blocked.action, "claimNext");
    assert!(!blocked.legal);
    assert!(blocked.blocked_by_active);
    assert_eq!(blocked.pre_active_request_id, Some(100));
    assert_eq!(blocked.post_active_request_id, Some(100));
    assert_eq!(blocked.pre_pending_request_ids, vec![101]);
    assert_eq!(blocked.post_pending_request_ids, vec![101]);
    assert_eq!(blocked.claimed_request_id, None);
    assert!(blocked.post_terminal_request_ids.is_empty());

    let terminal =
        lean_queue_deadline_case("terminal_active_allows_next_pending_same_session_claim");
    assert_eq!(terminal.group, "queue_admission");
    assert_eq!(terminal.action, "finishActive_then_claimNext");
    assert!(terminal.legal);
    assert!(!terminal.blocked_by_active);
    assert_eq!(terminal.pre_active_request_id, Some(100));
    assert_eq!(terminal.pre_pending_request_ids, vec![101]);
    assert_eq!(terminal.post_active_request_id, Some(101));
    assert_eq!(terminal.claimed_request_id, Some(101));
    assert!(terminal.post_pending_request_ids.is_empty());
    assert_eq!(terminal.post_terminal_request_ids, vec![100]);

    let coalesced =
        lean_queue_deadline_case("background_completion_session_coalesces_one_pending_wakeup");
    assert_eq!(coalesced.group, "queue_coalesce");
    assert_eq!(coalesced.action, "coalescePending_twice");
    assert!(coalesced.legal);
    assert_eq!(
        coalesced.queue_key.as_deref(),
        Some("background_completion:900")
    );
    assert!(coalesced.pre_pending_request_ids.is_empty());
    assert_eq!(coalesced.post_pending_request_ids, vec![201]);
    assert_eq!(coalesced.post_coalesced_pending_count, 1);
    assert!(coalesced.post_terminal_request_ids.is_empty());

    let cancel = lean_queue_deadline_case("cancel_drains_automated_wakeups_preserves_user_pending");
    assert_eq!(cancel.group, "queue_cancel");
    assert_eq!(cancel.action, "drainAutomated");
    assert!(cancel.legal);
    assert_eq!(
        cancel.queue_key.as_deref(),
        Some("background_completion:900")
    );
    assert_eq!(cancel.pre_pending_request_ids, vec![301, 302]);
    assert_eq!(cancel.post_pending_request_ids, vec![302]);
    assert_eq!(cancel.automated_drained_request_ids, vec![301]);
    assert_eq!(cancel.preserved_user_pending_request_ids, vec![302]);
    assert_eq!(cancel.post_terminal_request_ids, vec![301]);
    assert_eq!(cancel.post_coalesced_pending_count, 0);

    let deadline = lean_queue_deadline_case("claim_preserves_explicit_deadline");
    assert_eq!(deadline.group, "claim_deadline");
    assert_eq!(deadline.action, "claim");
    assert!(deadline.legal);
    assert_eq!(deadline.claimed_request_id, Some(401));
    assert_eq!(deadline.pre_request_deadline, Some(50));
    assert_eq!(deadline.synthesized_claim_deadline, Some(51));
    assert_eq!(deadline.post_deadline, Some(50));
    assert!(
        deadline.post_deadline < deadline.synthesized_claim_deadline,
        "explicit request deadline should remain tighter than the synthesized claim deadline"
    );
    assert!(deadline.explicit_deadline_preserved);
}

#[test]
fn generated_command_policy_cases_cover_policy_sandbox_and_env_contracts() {
    let forbidden = lean_command_policy_case("forbidden_prefix_wins_over_allowed_prefix_order");
    assert_eq!(forbidden.category, "forbidden_prefix");
    assert_eq!(forbidden.decision, "deny");
    assert_eq!(forbidden.denial_reason.as_deref(), Some("forbiddenPrefix"));
    assert_eq!(
        forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string()])
    );
    let second_forbidden = lean_command_policy_case("forbidden_prefix_second_configured_match");
    assert_eq!(
        second_forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string(), "diff".to_string()])
    );

    let allowed =
        lean_command_policy_case("allowed_prefix_required_precedes_network_and_allowlist");
    assert_eq!(allowed.decision, "deny");
    assert_eq!(
        allowed.denial_reason.as_deref(),
        Some("allowedPrefixRequired")
    );
    assert_eq!(
        allowed.denied_argv.as_ref(),
        Some(&vec!["curl".to_string(), "https://example.com".to_string()])
    );

    let curl = lean_command_policy_case("disabled_network_read_only_curl_denies_before_allowlist");
    assert_eq!(
        curl.denial_reason.as_deref(),
        Some("disabledNetworkCommand")
    );
    assert_eq!(curl.denied_command.as_deref(), Some("curl"));

    let workspace = lean_command_sandbox_case("workspace_write_enforced_selects_macos_seatbelt");
    assert_eq!(workspace.decision, "selected");
    assert_eq!(workspace.sandbox.as_deref(), Some("macos_seatbelt"));

    let unrestricted = lean_command_sandbox_case("unrestricted_selects_unsandboxed_unrestricted");
    assert_eq!(
        unrestricted.sandbox.as_deref(),
        Some("unsandboxed_unrestricted")
    );

    let key = lean_command_env_case("env_key_marker_dropped");
    assert_eq!(key.input_name, "OPENAI_API_KEY");
    assert_eq!(key.expected_output_value, None);

    let pager = lean_command_env_case("env_pager_forced_cat");
    assert_eq!(pager.expected_output_value.as_deref(), Some("cat"));
    let pager_absent = lean_command_env_case("env_pager_absent_still_forced_cat");
    assert_eq!(pager_absent.expected_output_value.as_deref(), Some("cat"));
}
