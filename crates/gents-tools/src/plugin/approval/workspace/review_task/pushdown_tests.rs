use gents_documents::{Task, TaskHook};
use gents_lean_vocab::families::access_review_selected_task_eligibility::SelectedTaskEligibility;
use gents_lean_vocab::families::access_review_task_eligibility::{TaskEligibility, TaskInput};

fn document(input: &TaskInput) -> Task {
    let document = &input.document;
    Task {
        node_did: document.node_did.clone(),
        task_id: document.task_id.clone(),
        display_name: None,
        description: None,
        agent_did: document.agent_did.clone(),
        prompt_template: document.prompt_template.clone(),
        emit_outcome: document.emit_outcome,
        outcome_readers: Vec::new(),
        goal_objective_template: document.goal_objective_template.clone(),
        goal_token_budget: document
            .goal_token_budget
            .map(|budget| i64::try_from(budget).unwrap()),
        hooks: document
            .hooks
            .iter()
            .map(|hook| TaskHook {
                hook_id: hook.hook_id.clone(),
                phase: serde_json::from_value(serde_json::Value::String(hook.phase.clone()))
                    .unwrap(),
                command: hook.command.clone(),
                timeout_secs: None,
                skip_exit_codes: Vec::new(),
            })
            .collect(),
        enabled: document.enabled,
        output_schema_ref: document.output_schema_ref.clone(),
        created_at: None,
        updated_at: None,
        tags: Vec::new(),
    }
}

#[test]
fn access_review_task_eligibility_matches_lean() {
    gents_lean_vocab::families::access_review_task_eligibility::replay(|input| TaskEligibility {
        eligible: crate::plugin::approval::workspace::review_task_is_eligible(
            &document(input),
            &input.node,
            &input.task_id,
            &input.agent,
        ),
    });
}

#[test]
fn access_review_selected_task_eligibility_matches_lean() {
    gents_lean_vocab::families::access_review_selected_task_eligibility::replay(
        |input: &gents_lean_vocab::families::access_review_selected_task_eligibility::TaskInput| {
            let input: TaskInput = gents_lean_vocab::family::recode(input);
            let document = document(&input);
            let mut approved = document.clone();
            approved.task_id = input.task_id.clone();
            approved.prompt_template = input.approved_prompt.clone();
            let selection =
                crate::plugin::approval::workspace::ReviewTaskSelection::for_task(&approved);
            SelectedTaskEligibility {
                selected_eligible:
                    crate::plugin::approval::workspace::review_task_matches_selection(
                        &document,
                        &selection,
                        &input.node,
                        &input.agent,
                    ),
            }
        },
    );
}
