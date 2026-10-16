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
    for case in &lean_contract_snapshot().fleet_slot_accounting_cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Fleet slot case {} emitted mismatched projection row arrays",
            case.name
        );
        if case.property == "admission_contribution" {
            assert!(
                matches!(
                    case.admission_state.as_str(),
                    "waiting" | "acquired" | "executing" | "released"
                ),
                "Fleet admission case {} must carry an admission phase of an existing claimed request, got {:?}",
                case.name,
                case.admission_state
            );
        } else {
            assert_eq!(
                case.property, "fleet_reconstructed_running_bound",
                "Fleet slot case {} has an unknown property",
                case.name
            );
            assert!(case.admission_state.is_empty(), "{}", case.name);
        }

        let reconstructed = reconstructed_running_slot_count(
            slot_rows_from_contract(&case.row_backend_ids, &case.row_states),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "Fleet slot case {} drifted from the Rust admission reconstruction of the projected rows",
            case.name
        );
        assert_eq!(
            reconstructed, case.slot_count,
            "Fleet slot case {} drifted from the Lean slotCountFor aggregate over the same rows",
            case.name
        );
        assert_eq!(
            case.bounded_by_max_concurrent,
            reconstructed <= case.max_concurrent,
            "Fleet slot case {} drifted from the max_concurrent capacity bound recomputed by the Rust count owner",
            case.name
        );
    }
}
