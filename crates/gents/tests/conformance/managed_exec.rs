//! ManagedExec conformance home: pins the generated native-process state
//! machine and liveness cases. The OS-process behavior behind each transition
//! (spawn/kill/reap against real children) is witnessed by the
//! `managed_exec::tests` unit suite, which the coverage ledger registers
//! alongside this shape fence.

use super::*;

pub(super) fn managed_exec_liveness_cases_pin_native_process_boundary() {
    let machine = lean_state_machine_contract("ManagedExec");
    assert_eq!(
        machine.states,
        vec![
            "pendingSpawn",
            "running",
            "exited",
            "killSignaled",
            "killed",
            "spawnFailed",
            "reapFailed"
        ]
    );
    assert!(machine
        .legal_transitions
        .iter()
        .any(|pair| pair.from == "running" && pair.to == "killSignaled"));
    assert!(machine
        .legal_transitions
        .iter()
        .any(|pair| pair.from == "killSignaled" && pair.to == "killed"));

    let cases = lean_managed_exec_liveness_cases();
    assert_eq!(cases.len(), 5);
    let deadline = cases
        .iter()
        .find(|case| case.name == "running_child_expired_deadline_kill_signaled")
        .expect("deadline liveness case must be emitted");
    assert_eq!(deadline.trigger, "deadlineElapsed");
    assert_eq!(deadline.pre_exec_state, "running");
    assert_eq!(deadline.pre_tool_state, "running");
    assert_eq!(deadline.expected_exec_state, "killSignaled");
    assert_eq!(deadline.expected_tool_state, "timedOut");
    assert_eq!(deadline.max_steps, 1);
    assert!(deadline.kill_signal_required);

    let cancel = cases
        .iter()
        .find(|case| case.name == "running_child_cancel_kill_signaled")
        .expect("cancel liveness case must be emitted");
    assert_eq!(cancel.trigger, "cancelRequested");
    assert_eq!(cancel.expected_tool_state, "cancelled");
    assert!(cancel.kill_signal_required);

    for case in cases {
        if case.expected_exec_state == "killSignaled" {
            assert!(
                case.kill_signal_required,
                "kill-signaled cases must require an OS signal: {case:?}"
            );
        } else {
            assert!(
                !case.kill_signal_required,
                "non-kill cases must not require an OS signal: {case:?}"
            );
        }
    }
}
