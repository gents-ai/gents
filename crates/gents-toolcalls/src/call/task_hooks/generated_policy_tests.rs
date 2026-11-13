use crate::call::task_hooks::effective_timeout_secs;
use crate::call::task_hooks::test_support::lean_hook_phase;
use gents_documents::{Task, TaskHook, TaskHookPhase};
use gents_lean_vocab::lean_task_hook_run_cases;

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

fn modeled_hook(
    hook: &gents_lean_vocab::families::task_hooks_contracts_effective_timeout::TaskHook<TaskHookPhase>,
) -> TaskHook {
    TaskHook {
        hook_id: hook.hook_id.clone(),
        phase: hook.phase,
        command: hook.command.clone(),
        timeout_secs: hook.timeout_secs,
        skip_exit_codes: hook.skip_exit_codes.clone(),
    }
}

#[test]
fn generated_task_hook_timeouts_follow_the_model() {
    gents_lean_vocab::families::task_hooks_contracts_effective_timeout::replay::<TaskHookPhase>(
        |hook| gents_lean_vocab::families::task_hooks_contracts_effective_timeout::TimeoutOutput {
            effective_timeout_secs: effective_timeout_secs(&modeled_hook(hook)),
        },
    );
}

#[test]
fn generated_task_hook_admission_cases_fence_production_validation() {
    gents_lean_vocab::families::task_hooks_contracts_admission::replay::<TaskHookPhase>(|case| {
        let task = task_with_hooks(
            &case.name,
            case.task_hooks
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
        gents_lean_vocab::families::task_hooks_contracts_admission::AdmissionOutput {
            admitted: task.validate().is_ok(),
        }
    });
}

#[test]
fn generated_task_hook_cases_fence_the_modeled_phase_vocabulary() {
    let run_cases = lean_task_hook_run_cases();
    assert!(
        !run_cases.is_empty(),
        "task hook run cases must not be empty"
    );

    let emitted: std::collections::BTreeSet<&str> = run_cases
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

    for (name, hooks) in run_cases
        .iter()
        .map(|case| (&case.name, &case.hooks))
    {
        let task = task_with_hooks(
            name,
            hooks
                .iter()
                .map(|hook| crate::call::task_hooks::test_support::native_task_hook(hook))
                .collect(),
        );
        assert!(
            task.validate().is_ok(),
            "{name}: generated trace uses hooks production validation rejects: {:?}",
            task.validate().err(),
        );
    }
}
