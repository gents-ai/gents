use crate::call::task_hooks::effective_timeout_secs;
use crate::call::task_hooks::test_support::lean_hook_phase;
use gents_documents::{Task, TaskHook};
use gents_lean_vocab::{lean_task_hook_admission_cases, lean_task_hook_recovery_cases};

fn task_with_hooks(task_id: &str, hooks: Vec<TaskHook>) -> Task {
    Task {
        emit_outcome: false,
        outcome_readers: Vec::new(),
        node_did: "did:key:z6MkTaskHookConformance".to_string(),
        task_id: task_id.to_string(),
        display_name: None,
        description: None,
        agent_did: "agent-1".to_string(),
        prompt_template: "run the task".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        hooks,
        enabled: true,
        output_schema_ref: None,
        created_at: None,
        updated_at: None,
        tags: Vec::new(),
    }
}

#[test]
fn generated_task_hook_admission_cases_fence_production_validation() {
    let cases = lean_task_hook_admission_cases();
    assert!(
        !cases.is_empty(),
        "task hook admission conformance cases must not be empty"
    );
    for case in cases {
        for generated in &case.hooks {
            assert_eq!(
                effective_timeout_secs(&crate::call::task_hooks::test_support::native_task_hook(
                    generated
                )),
                generated.effective_timeout_secs,
                "{}: production timeout resolution disagrees with the model for {:?}",
                case.name,
                generated.hook_id
            );
        }
        let task = task_with_hooks(
            &case.name,
            case.hooks
                .iter()
                .map(|hook| crate::call::task_hooks::test_support::native_task_hook(hook))
                .collect(),
        );
        assert_eq!(
            task.validate().is_ok(),
            case.expected_admitted,
            "{}: production Task::validate disagrees with TaskHooks.admitHooks ({:?})",
            case.name,
            task.validate().err(),
        );
    }
}

#[test]
fn generated_task_hook_run_traces_are_admitted_by_production_validation() {
    gents_lean_vocab::families::task_hooks_contracts_run_admitted::replay::<
        String,
        String,
        gents_documents::TaskHookPhase,
    >(|i| {
        let task = task_with_hooks(
            &i.name,
            i.hooks
                .iter()
                .map(|hook| TaskHook {
                    hook_id: hook.hook_id.clone(),
                    phase: hook.phase,
                    command: hook.command.clone(),
                    timeout_secs: hook.timeout_secs,
                    skip_exit_codes: hook.skip_exit_codes.clone(),
                })
                .collect(),
        );
        gents_lean_vocab::families::task_hooks_contracts_run_admitted::AdmittedOutput {
            admitted: task.validate().is_ok(),
        }
    });
}

#[test]
fn generated_task_hook_cases_fence_the_modeled_phase_vocabulary() {
    let recovery_cases = lean_task_hook_recovery_cases();
    assert!(
        !recovery_cases.is_empty(),
        "task hook recovery cases must not be empty"
    );

    let emitted: std::collections::BTreeSet<&str> = recovery_cases
        .iter()
        .flat_map(|case| case.hooks.iter())
        .map(|generated| generated.phase.as_str())
        .collect();
    assert_eq!(
        emitted,
        ["after_failure", "after_success", "before", "finally"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
    );
    for name in &emitted {
        let encoded = serde_json::to_string(&lean_hook_phase(name)).expect("phase serializes");
        assert_eq!(encoded, format!("\"{name}\""));
    }

    for case in recovery_cases {
        let task = task_with_hooks(
            &case.name,
            case.hooks
                .iter()
                .map(|hook| crate::call::task_hooks::test_support::native_task_hook(hook))
                .collect(),
        );
        assert!(
            task.validate().is_ok(),
            "{}: generated trace uses hooks production validation rejects: {:?}",
            case.name,
            task.validate().err(),
        );
    }
}
