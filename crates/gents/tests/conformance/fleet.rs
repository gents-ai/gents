//! Fleet conformance home: generated inference/fleet slot-accounting
//! contract rows over the derived InferenceCall projection.

use super::*;

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
