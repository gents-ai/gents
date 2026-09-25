use crate::document_config::{Task, TaskHook, TaskHookPhase};
use crate::lean_vocab_test::{
    lean_task_hook_admission_cases, lean_task_hook_recovery_cases, lean_task_hook_run_cases,
    LeanTaskHook,
};

fn phase(name: &str) -> TaskHookPhase {
    match name {
        "before" => TaskHookPhase::Before,
        "after_success" => TaskHookPhase::AfterSuccess,
        "after_failure" => TaskHookPhase::AfterFailure,
        "finally" => TaskHookPhase::Finally,
        other => panic!("generated contract emitted an unknown hook phase {other:?}"),
    }
}

fn hook(generated: &LeanTaskHook) -> TaskHook {
    TaskHook {
        hook_id: generated.hook_id.clone(),
        phase: phase(&generated.phase),
        command: generated.command.clone(),
        timeout_secs: generated.timeout_secs,
    }
}

fn task_with_hooks(task_id: &str, hooks: Vec<TaskHook>) -> Task {
    Task {
        agent_did: "did:key:z6MkTaskHookConformance".to_string(),
        task_id: task_id.to_string(),
        display_name: None,
        description: None,
        behavior_id: "behavior-1".to_string(),
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
        let task = task_with_hooks(&case.name, case.hooks.iter().map(hook).collect());
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
fn generated_task_hook_cases_fence_the_modeled_phase_vocabulary() {
    let run_cases = lean_task_hook_run_cases();
    let recovery_cases = lean_task_hook_recovery_cases();
    assert!(!run_cases.is_empty(), "task hook run cases must not be empty");
    assert!(
        !recovery_cases.is_empty(),
        "task hook recovery cases must not be empty"
    );

    let emitted: std::collections::BTreeSet<&str> = run_cases
        .iter()
        .flat_map(|case| case.hooks.iter())
        .chain(recovery_cases.iter().flat_map(|case| case.hooks.iter()))
        .map(|generated| generated.phase.as_str())
        .collect();
    assert_eq!(
        emitted,
        ["after_failure", "after_success", "before", "finally"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
    );
    for name in &emitted {
        let encoded = serde_json::to_string(&phase(name)).expect("phase serializes");
        assert_eq!(encoded, format!("\"{name}\""));
    }

    for (name, hooks) in run_cases
        .iter()
        .map(|case| (&case.name, &case.hooks))
        .chain(recovery_cases.iter().map(|case| (&case.name, &case.hooks)))
    {
        let task = task_with_hooks(name, hooks.iter().map(hook).collect());
        assert!(
            task.validate().is_ok(),
            "{name}: generated trace uses hooks production validation rejects: {:?}",
            task.validate().err(),
        );
    }
}
