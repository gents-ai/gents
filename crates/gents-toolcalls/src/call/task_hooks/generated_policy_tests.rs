use crate::call::task_hooks::effective_timeout_secs;
use gents_documents::{Task, TaskHook, TaskHookPhase};

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
    hook: &gents_lean_vocab::families::task_hooks_contracts_effective_timeout::TaskHook<
        TaskHookPhase,
    >,
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
