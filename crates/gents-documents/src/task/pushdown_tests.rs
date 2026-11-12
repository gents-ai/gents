use super::Task;

#[test]
fn task_document_round_trips_omitted_budget_as_none() {
    let task: Task = serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zNODE",
        "task_id": "contract-task",
        "agent_did": "contract",
        "prompt_template": "do the thing",
        "goal_objective_template": "ship the capability contract",
        "goal_token_budget": null
    }))
    .expect("task document with explicit null budget");
    assert_eq!(task.goal_token_budget, None);
    assert_eq!(
        task.goal_objective_template.as_deref(),
        Some("ship the capability contract")
    );

    let omitted: Task = serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zNODE",
        "task_id": "contract-task",
        "agent_did": "contract",
        "prompt_template": "do the thing"
    }))
    .expect("task document without any goal fields");
    assert_eq!(omitted.goal_token_budget, None);
    assert_eq!(omitted.goal_objective_template, None);
}
