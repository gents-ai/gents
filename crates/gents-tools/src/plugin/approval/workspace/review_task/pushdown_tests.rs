use gents_documents::{Task, TaskHook};

#[test]
fn generated_task_policies_bind_the_configured_task_fields() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().access_review_task_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let document = Task {
            node_did: case.document.node_did.clone(),
            task_id: case.document.task_id.clone(),
            display_name: None,
            description: None,
            agent_did: case.document.agent_did.clone(),
            prompt_template: case.document.prompt_template.clone(),
            emit_outcome: case.document.emit_outcome,
            outcome_readers: Vec::new(),
            goal_objective_template: case.document.goal_objective_template.clone(),
            goal_token_budget: case
                .document
                .goal_token_budget
                .map(|budget| i64::try_from(budget).unwrap()),
            hooks: case
                .document
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
            enabled: case.document.enabled,
            output_schema_ref: case.document.output_schema_ref.clone(),
            created_at: None,
            updated_at: None,
            tags: Vec::new(),
        };
        let mut approved = document.clone();
        approved.task_id = case.task_id.clone();
        approved.prompt_template = case.approved_prompt.clone();
        let selection =
            crate::plugin::approval::workspace::ReviewTaskSelection::for_task(&approved);
        assert_eq!(
            crate::plugin::approval::workspace::review_task_matches_selection(
                &document,
                &selection,
                &case.node,
                &case.agent,
            ),
            case.selected_eligible,
            "{}",
            serde_json::to_string(&document).unwrap(),
        );
        assert_eq!(
            crate::plugin::approval::workspace::review_task_is_eligible(
                &document,
                &case.node,
                &case.task_id,
                &case.agent
            ),
            case.eligible,
            "{}",
            serde_json::to_string(&document).unwrap()
        );
    }
}
