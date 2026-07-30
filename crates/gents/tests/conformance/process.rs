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
