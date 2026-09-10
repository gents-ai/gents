//! Fleet slot-accounting conformance: pins the generated fleet admission
//! projection rows to the real slot-count owner in
//! `crates/gents/src/admission/slot_accounting.rs`.
//!
//! The Lean `FleetState` model is admission-only: it admits requests that
//! already exist and were claimed through the common request entrance
//! (`acceptExisting` requires `claimed` + `waiting`); it never constructs
//! scheduled work or selects inference. Every emitted case therefore carries
//! one of the four admission phases of an existing claimed request
//! (`waiting` / `acquired` / `executing` / `released`), and the pin below
//! rejects any other phase from entering the contract.
//!
//! Ownership of the stronger guarantees deliberately lives elsewhere and is
//! not duplicated here:
//! - `admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary`
//!   replays every fleet case against real admission rows in DefraDB, and
//!   `conformance/inference_call.rs` drives the InferenceCall slot cases over
//!   persisted rows (the fleet aggregate is a derived view over those rows —
//!   `boundary.fleet-slot-accounting.derived-view`).
//! - The coverage ledger's shape/count and boundary classification are pinned
//!   by `conformance/coverage.rs`.
//!
//! This file keeps the cheap, always-on cross-language pin: reconstructing the
//! Lean-projected rows with the Rust count owner must equal both Lean
//! reconstructions (`reconstructedRunningCount` over the projected
//! InferenceCall rows and `slotCountFor` over the fleet state), and the
//! emitted capacity-bound flag must agree with the bound recomputed by the
//! Rust owner.

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
        assert!(
            matches!(
                case.admission_state.as_str(),
                "waiting" | "acquired" | "executing" | "released"
            ),
            "Fleet slot case {} must carry an admission phase of an existing claimed request, got {:?}",
            case.name,
            case.admission_state
        );

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
