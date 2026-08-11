use super::*;

pub(super) fn managed_exec_tool_boundary_cases_cover_every_native_subprocess_tool() {
    let cases = lean_managed_exec_tool_boundary_cases();
    let tools = cases
        .iter()
        .map(|case| (case.tool_name.as_str(), case.work_class.as_str()))
        .collect::<BTreeSet<_>>();

    assert_eq!(
        tools,
        BTreeSet::from([
            ("bash", "shellCommand"),
            ("bash_unrestricted", "shellCommand"),
            ("glob", "filesystemTraversal"),
            ("grep", "filesystemTraversal"),
            ("list_files", "filesystemTraversal"),
        ])
    );
    for case in cases {
        assert_eq!(
            case.name,
            format!(
                "{}_routes_through_managed_exec_process_tree_boundary",
                case.tool_name
            )
        );
        assert_eq!(case.boundary, "managedExecProcessGroupBoundary");
        assert_eq!(case.kill_scope, "processTree");
        assert!(case.timeout_requires_kill);
        assert!(case.cancel_requires_kill);
        assert!(case.descendants_in_termination_scope);
        assert!(case.capture_drain_bounded);
    }
}

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

    let nonzero = cases
        .iter()
        .find(|case| case.name == "nonzero_child_exit_fails_without_kill")
        .expect("nonzero-exit outcome case must be emitted");
    assert_eq!(nonzero.trigger, "observeExitFailure");
    assert_eq!(nonzero.expected_exec_state, "exited");
    assert_eq!(nonzero.expected_tool_state, "failed");
    assert!(!nonzero.kill_signal_required);

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
