//! Process conformance home: pins the daemon process-lifecycle state machine
//! and the generated transition-case partition. The DB-backed transition
//! drive (publishing process states through `RuntimeStatusHandle`) is the
//! ledger-registered `runtime_status::tests` consumer; this fence keeps the
//! emitted machine and its 25-case partition honest from the conformance
//! binary.

use super::*;

pub(super) fn generated_process_transition_cases_cover_runtime_status_policy_shape() {
    assert_state_machine_contract_is_complete("Process");

    let machine = lean_state_machine_contract("Process");
    let states = machine
        .states
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_lifecycle_transition_cases_partition(
        "Process",
        &states,
        lean_process_transition_cases(),
    );
}
