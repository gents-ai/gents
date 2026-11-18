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

fn task(fields: serde_json::Value) -> Task {
    let mut document = serde_json::json!({"node_did": "did:key:zNODE", "task_id": "tick"});
    document
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(document).expect("task document decodes")
}

fn refusal(fields: serde_json::Value) -> String {
    task(fields)
        .validate()
        .expect_err("task is refused")
        .to_string()
}

#[test]
fn a_task_runs_either_an_agent_or_a_command() {
    task(serde_json::json!({"agent_did": "did:key:zAGENT", "prompt_template": "tick"}))
        .validate()
        .expect("an agent Task is admitted as before");
    task(serde_json::json!({"command": {"argv": ["true"], "timeout_secs": 60}}))
        .validate()
        .expect("a command Task needs no agent");

    assert!(
        refusal(serde_json::json!({}))
            .contains("needs an agent (agent_did and prompt_template) or a command"),
        "a Task with neither is refused"
    );
    let both = refusal(serde_json::json!({
        "agent_did": "did:key:zAGENT", "prompt_template": "tick", "command": {"argv": ["true"]}
    }));
    assert!(
        both.contains("has a command and agent_did, prompt_template") && both.contains("drop them"),
        "a Task with both is refused naming the agent fields: {both}"
    );
    let hooks = refusal(serde_json::json!({
        "command": {"argv": ["true"]},
        "hooks": [{"hook_id": "h", "phase": "before", "command": ["true"]}]
    }));
    assert!(hooks.contains("has a command and hooks"), "{hooks}");
}

#[test]
fn a_command_names_a_program_and_a_bounded_timeout() {
    assert!(refusal(serde_json::json!({"command": {"argv": []}}))
        .contains("argv must start with a nonempty program"));
    assert!(refusal(serde_json::json!({"command": {"argv": [" "]}}))
        .contains("argv must start with a nonempty program"));
    for timeout in [0, 86_401] {
        assert!(refusal(
            serde_json::json!({"command": {"argv": ["true"], "timeout_secs": timeout}})
        )
        .contains("timeout_secs must be between 1 and 86400"));
    }
}

#[test]
fn an_agent_task_document_keeps_its_shape() {
    let document = serde_json::json!({
        "node_did": "did:key:zNODE", "task_id": "tick",
        "agent_did": "did:key:zAGENT", "prompt_template": "tick"
    });
    let decoded: Task = serde_json::from_value(document.clone()).unwrap();
    assert_eq!(decoded.command, None);
    assert_eq!(serde_json::to_value(&decoded).unwrap(), document);

    let stored_row: Task = serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zNODE", "task_id": "tick",
        "agent_did": null, "prompt_template": null,
        "command": {"argv": ["true"]}
    }))
    .expect("a stored command Task row reads its null agent columns as absent");
    assert_eq!(
        serde_json::to_value(&stored_row).unwrap(),
        serde_json::json!({"node_did": "did:key:zNODE", "task_id": "tick", "command": {"argv": ["true"]}})
    );
}
