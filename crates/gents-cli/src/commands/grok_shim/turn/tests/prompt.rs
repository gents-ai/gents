use super::*;

#[tokio::test]
async fn prompt_resolves_end_turn_after_terminalization() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let engine = test_engine(node.clone());
    let (_buffer, sender) = buffer_sender();

    let prompt = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("hello")],
            "_meta": {"promptId": "prompt-1"},
        }),
        Some(json!(1)),
    )
    .unwrap();

    let node_for_terminalize = node.clone();
    let did_for_terminalize = node_did.clone();
    let handle = tokio::spawn(async move {
        loop {
            let scope = gents::session::session_scope_filter(
                &did_for_terminalize,
                "session-1",
                Some(&did_for_terminalize),
            );
            let query = format!(
                "{{AgentRequest(filter: {{{scope}, lifecycle_state: {{_eq: \"pending\"}}}}) {{{}}}}}",
                gents::SIGNED_REQUEST_FIELDS
            );
            let response = node_for_terminalize.execute(&query).await;
            ensure_no_errors(&response, "pending fixture rows").unwrap();
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(Value::as_array)
                .cloned()
                .expect("pending fixture rows");
            assert!(rows.len() <= 1, "ambiguous pending fixture request");
            if let Some(row) = rows.first() {
                let receipt: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(row.clone()).unwrap();
                terminalize_request(&node_for_terminalize, &receipt, "completed").await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        manager.handle_prompt(prompt, &sender, &engine),
    )
    .await
    .expect("prompt should resolve within timeout")
    .expect("prompt should succeed");
    assert_eq!(result["stopReason"], json!("end_turn"));
    handle.await.expect("terminalize task");
}

#[tokio::test]
async fn goal_prompt_atomically_submits_scoped_goal_and_signed_request() {
    let (_dir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let mut prompt = parse_prompt_request(
        &json!({
            "sessionId":"goal-submit-session",
            "prompt":[text_block("/goal Explain the architecture --budget 100000")],
            "_meta":{"screenMode":"inline", "sendNow":true}
        }),
        None,
    )
    .unwrap();
    let id_receipt = manager
        .submit_request(&prompt, "goal-prompt-id")
        .await
        .unwrap();
    let id = id_receipt.request_id.clone();
    let goal = gents::goal::load_canonical_goal(&node, &node_did, &prompt.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.objective, "Explain the architecture");
    assert_eq!(goal.status, "active");
    assert_eq!(goal.token_budget, Some(100000));
    let response = node.execute(&format!(
        "{{AgentRequest(filter:{{request_id:{{_eq:\"{id}\"}}}}){{request_id node_did session_id content input retry_key admission_signer_did}}}}"
    )).await;
    gents::graphql::ensure_no_errors(&response, "goal submission").unwrap();
    let row = &response.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(row["content"], "Explain the architecture");
    assert_eq!(row["node_did"], node_did);
    assert_eq!(row["admission_signer_did"], node_did);
    assert_eq!(row["session_id"], prompt.session_id);
    assert_eq!(row["retry_key"], format!("goal-request:{id}"));
    assert!(row["input"].is_null());

    prompt.prompt[0].text = "/goal Conflicting objective".into();
    assert!(manager.submit_request(&prompt, "conflict").await.is_err());
    let response = node
        .execute("{AgentRequest{request_id} Goal{goal_id} GoalCreationClaim{creation_key}}")
        .await;
    gents::graphql::ensure_no_errors(&response, "atomic rejection").unwrap();
    let data = response.data.as_ref().unwrap();
    for collection in ["AgentRequest", "Goal", "GoalCreationClaim"] {
        assert_eq!(
            data[collection].as_array().unwrap().len(),
            1,
            "{collection}"
        );
    }
    assert_eq!(
        gents::goal::load_canonical_goal(&node, &node_did, &prompt.session_id)
            .await
            .unwrap()
            .unwrap()
            .objective,
        "Explain the architecture"
    );
    gents::goal::delete_goals_for_session(&node, &node_did, &prompt.session_id)
        .await
        .unwrap();
    let next_receipt = manager
        .submit_request(&prompt, "new-incarnation")
        .await
        .unwrap();
    let next = next_receipt.request_id.clone();
    assert_ne!(id, next);
}

#[tokio::test]
async fn live_turn_streams_tool_agent_and_message_updates_once_each() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = Arc::new(TurnManager::new(
        node.clone(),
        test_config(graphql, &node_did),
    ));
    let engine = test_engine(node.clone());
    let (buffer, sender) = buffer_sender();

    let prompt = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("hello")],
            "_meta": {"promptId": "prompt-1"},
        }),
        Some(json!(1)),
    )
    .unwrap();

    let node_for_seed = node.clone();
    let did_for_seed = node_did.clone();
    let seed_handle = tokio::spawn(async move {
        let request_id = wait_for_pending_request(&node_for_seed, &did_for_seed).await;
        let tool_doc = seed_tool_call(
            &node_for_seed,
            &request_id,
            "call-1",
            "agent_new",
            "running",
            "",
        )
        .await;
        let child = seed_child_request(
            &node_for_seed,
            &request_id,
            &tool_doc,
            "child-1",
            "processing",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        complete_tool_call(&node_for_seed, &tool_doc, "file contents").await;
        complete_child_request(&node_for_seed, &child).await;
        seed_assistant_message(&node_for_seed, &request_id, 1, "the answer").await;
        terminalize_request(&node_for_seed, &request_id, "completed").await;
    });

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        manager.handle_prompt(prompt, &sender, &engine),
    )
    .await
    .expect("prompt should resolve within timeout")
    .expect("prompt should succeed");
    assert_eq!(result["stopReason"], json!("end_turn"));
    seed_handle.await.expect("seed task");

    let kinds = buffered_update_kinds(&buffer).await;
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("user_message_chunk")
    );
    for expected in [
        "tool_call",
        "tool_call_update",
        "available_commands_update",
        "subagent_spawned",
        "subagent_progress",
        "subagent_finished",
        "agent_message_chunk",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "expected a {expected} notification, got: {kinds:?}"
        );
    }
    for kind in [
        "tool_call",
        "available_commands_update",
        "subagent_spawned",
        "subagent_finished",
        "agent_message_chunk",
    ] {
        let count = kinds.iter().filter(|k| k == &kind).count();
        assert_eq!(count, 1, "{kind} must stream exactly once, got {count}");
    }
    let updates = parse_buffered_updates(&buffer).await;
    let revisions: Vec<_> = updates
        .iter()
        .filter_map(|event| {
            let update = event.pointer("/params/update")?;
            (update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call_update"))
                .then_some(update)
        })
        .collect();
    assert_eq!(
        revisions,
        vec![
            &json!({
                "sessionUpdate": "tool_call_update", "toolCallId": "call-1",
                "content": [{"type": "content", "content": {"type": "text", "text": "file contents"}}],
                "rawOutput": {"output": "file contents"},
            }),
            &json!({
                "sessionUpdate": "tool_call_update", "toolCallId": "call-1",
                "status": "completed",
            }),
        ]
    );
    assert!(
        updates.len() >= 8,
        "expected the full stream, got {updates:?}"
    );
    for update in &updates {
        let kind = update["params"]["update"]["sessionUpdate"]
            .as_str()
            .expect("sessionUpdate kind");
        let expected_method = if kind.starts_with("subagent_") {
            "_x.ai/session_notification"
        } else {
            "session/update"
        };
        assert_eq!(update["method"], expected_method);
        assert_eq!(update["params"]["sessionId"], "session-1");
        assert_eq!(update["params"]["_meta"]["promptId"], "prompt-1");
        assert!(
            update["params"]["_meta"]["eventId"].as_str().is_some(),
            "every notification carries an eventId"
        );
    }
}

#[tokio::test]
async fn identical_text_distinct_rows_both_stream() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let engine = test_engine(node.clone());
    let (buffer, sender) = buffer_sender();

    let prompt = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("hello")],
            "_meta": {"promptId": "prompt-1"},
        }),
        Some(json!(1)),
    )
    .unwrap();

    let node_for_seed = node.clone();
    let did_for_seed = node_did.clone();
    let seed_handle = tokio::spawn(async move {
        let request_id = wait_for_pending_request(&node_for_seed, &did_for_seed).await;
        seed_assistant_message(&node_for_seed, &request_id, 1, "same text").await;
        seed_assistant_message(&node_for_seed, &request_id, 2, "same text").await;
        terminalize_request(&node_for_seed, &request_id, "completed").await;
    });

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        manager.handle_prompt(prompt, &sender, &engine),
    )
    .await
    .expect("prompt should resolve within timeout")
    .expect("prompt should succeed");
    assert_eq!(result["stopReason"], json!("end_turn"));
    seed_handle.await.expect("seed task");

    let updates = parse_buffered_updates(&buffer).await;
    let chunks: Vec<&str> = updates
        .iter()
        .filter(|update| update["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|update| update["params"]["update"]["content"]["text"].as_str())
        .collect();
    assert_eq!(
        chunks,
        vec!["same text", "same text"],
        "both distinct rows must stream even with identical text"
    );
}
