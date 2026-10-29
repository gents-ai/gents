use super::*;

#[tokio::test]
async fn resume_hands_off_active_and_during_replay_requests_without_repeating_history() {
    let (_dir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let engine = test_engine(node.clone());
    let (buffer, sender) = buffer_sender();
    let prompt = parse_prompt_request(
        &json!({"sessionId":"session-1", "prompt":[text_block("Original prompt")]}),
        None,
    )
    .unwrap();
    let first_receipt = manager.submit_request(&prompt, "first").await.unwrap();
    let first = first_receipt.request_id.clone();
    seed_assistant_message(&node, &first_receipt, 1, "Before reconnect.").await;
    let attached = chrono::Utc::now().to_rfc3339();
    let rows = super::super::super::sessions::requests(&node, &node_did, Some("session-1"))
        .await
        .unwrap();
    let running = manager
        .replay_session("session-1", &rows, &sender, &engine)
        .await
        .unwrap();
    assert_eq!(running, Some(format!("notifications-{first}")));
    let historical = parse_buffered_updates(&buffer).await;
    assert!(historical
        .iter()
        .all(|event| event["params"]["_meta"]["isReplay"] == true));
    buffer.lock().await.clear();
    seed_assistant_message(&node, &first_receipt, 2, "After reconnect.").await;
    terminalize_request(&node, &first_receipt, "completed").await;
    let second_receipt = manager.submit_request(&prompt, "second").await.unwrap();
    seed_assistant_message(&node, &second_receipt, 3, "Created during replay.").await;
    terminalize_request(&node, &second_receipt, "completed").await;
    let mut after = String::new();
    let mut delivery_after = String::new();
    for _ in 0..3 {
        manager
            .observe_session_tick(
                "session-1",
                &attached,
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
    }
    let updates = parse_buffered_updates(&buffer).await;
    let text = updates
        .iter()
        .filter_map(|event| {
            event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        })
        .collect::<String>();
    assert!(!text.contains("Before reconnect."));
    assert_eq!(text.matches("After reconnect.").count(), 1);
    assert_eq!(text.matches("Created during replay.").count(), 1);
    assert!(updates
        .iter()
        .all(|event| event["params"]["_meta"]["isReplay"] != true));
    assert!(manager.autonomous_delivery.lock().await.is_empty());
}

#[tokio::test]
async fn session_discovery_excludes_foreign_nodes_and_requesters() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let engine = test_engine(node.clone());
    let (_, sender) = buffer_sender();
    for (id, owner, requester) in [
        ("foreign-agent", "did:test:foreign", Some(node_did.as_str())),
        (
            "foreign-requester",
            node_did.as_str(),
            Some("did:test:foreign"),
        ),
        ("missing-requester", node_did.as_str(), None),
        ("owned", node_did.as_str(), Some(node_did.as_str())),
    ] {
        let requester = requester
            .map(|did| format!("\"{}\"", escape_graphql_string(did)))
            .unwrap_or_else(|| "null".into());
        let result = node
            .execute(&format!(
                r#"mutation {{ create_AgentRequest(input: {{
            request_id: "{id}", purpose: "normal", session_id: "session-1", node_did: "{}",
            requester_did: {requester}, lifecycle_state: "completed",
            created_at: "2026-06-04T12:00:00Z"
        }}) {{ _docID }} }}"#,
                escape_graphql_string(owner)
            ))
            .await;
        ensure_no_errors(&result, "seed scoped session discovery").unwrap();
    }
    manager
        .observe_session_tick(
            "session-1",
            "1970-01-01T00:00:00Z",
            &mut String::new(),
            &mut String::new(),
            &sender,
            &engine,
        )
        .await
        .unwrap();
    let observed = manager.observed.lock().await;
    assert_eq!(observed.len(), 1);
    assert!(observed.contains_key(&("session-1".into(), "owned".into())));
}

#[tokio::test]
async fn observed_human_turn_keeps_prompt_identity_echo_and_cancel_target() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let engine = test_engine(node.clone());
    let (buffer, sender) = buffer_sender();
    let prompt = parse_prompt_request(
        &json!({"sessionId":"session-1", "prompt":[text_block("Human from another client")]}),
        None,
    )
    .unwrap();
    let request_receipt = manager.submit_request(&prompt, "peer-human").await.unwrap();
    let request = request_receipt.request_id.clone();
    seed_assistant_message(&node, &request_receipt, 1, "Peer answer").await;
    let mut after = String::new();
    let mut delivery_after = String::new();
    manager
        .observe_session_tick(
            "session-1",
            "1970-01-01T00:00:00Z",
            &mut after,
            &mut delivery_after,
            &sender,
            &engine,
        )
        .await
        .unwrap();
    let events: Vec<Value> = buffer
        .lock()
        .await
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let echo = events
        .iter()
        .find(|event| event["params"]["update"]["sessionUpdate"] == "user_message_chunk")
        .unwrap();
    assert_eq!(echo["params"]["_meta"]["promptId"], request);
    assert_eq!(
        echo["params"]["update"]["_meta"]["hideFromScrollback"],
        false
    );
    assert_eq!(
        echo["params"]["update"]["content"]["text"],
        "Human from another client"
    );
    for target in ["different-prompt", request.as_str()] {
        manager
            .handle_cancel(
                parse_cancel_notification(
                    &json!({"sessionId":"session-1", "_meta":{"promptId":target}}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let interrupted = node
            .execute(&format!(
                r#"{{AgentRequest(filter:{{request_id:{{_eq:"{}"}}}}){{interrupt_requested_at}}}}"#,
                escape_graphql_string(&request)
            ))
            .await;
        ensure_no_errors(&interrupted, "read viewer cancel intent").unwrap();
        assert_eq!(
            interrupted.data.as_ref().unwrap()["AgentRequest"][0]["interrupt_requested_at"]
                .as_str()
                .is_some(),
            target == request
        );
    }
    terminalize_request(&node, &request_receipt, "interrupted").await;
    manager
        .observe_session_tick(
            "session-1",
            "1970-01-01T00:00:00Z",
            &mut after,
            &mut delivery_after,
            &sender,
            &engine,
        )
        .await
        .unwrap();
    let events: Vec<Value> = buffer
        .lock()
        .await
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["params"]["update"]["sessionUpdate"] == "user_message_chunk")
            .count(),
        1
    );
    let completed = events
        .iter()
        .find(|event| event["params"]["update"]["sessionUpdate"] == "turn_completed")
        .unwrap();
    assert_eq!(completed["params"]["update"]["prompt_id"], request);
    assert_eq!(completed["params"]["update"]["stop_reason"], "cancelled");
}

#[tokio::test]
async fn autonomous_turn_defers_late_notices_and_plain_escape_cancels_its_owner() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let engine = test_engine(node.clone());
    let (buffer, sender) = buffer_sender();
    let prompt = parse_prompt_request(
        &json!({"sessionId": "session-1",
        "prompt": [text_block("internal wake instruction")]}),
        None,
    )
    .unwrap();
    let root_receipt = manager.submit_request(&prompt, "root").await.unwrap();
    let root = root_receipt.request_id.clone();
    terminalize_request(&node, &root_receipt, "completed").await;
    manager.observed.lock().await.insert(
        ("session-1".into(), root.clone()),
        Arc::new(Mutex::new(ObservedRequest::new("root".into(), 0, true))),
    );
    let first_receipt = seed_runtime_wake(&node, &node_did, "internal wake instruction").await;
    let first = first_receipt.request_id.clone();
    seed_assistant_message(&node, &first_receipt, 1, "Wake A is working.").await;
    seed_tool_call(&node, &first_receipt, "wake-a-tool", "bash", "running", "").await;
    let mut after = String::new();
    let mut delivery_after = String::new();
    manager
        .observe_session_tick(
            "session-1",
            "1970-01-01T00:00:00Z",
            &mut after,
            &mut delivery_after,
            &sender,
            &engine,
        )
        .await
        .unwrap();
    assert_eq!(
        manager.autonomous_delivery.lock().await.get("session-1"),
        Some(&first)
    );

    for (request, sequence, text) in [
        (&root_receipt, 2, "Root task finished."),
        (&first_receipt, 3, "Wake task finished."),
    ] {
        seed_completion_notice(&node, request, sequence, text).await;
    }
    let second_receipt = seed_runtime_wake(&node, &node_did, "internal wake instruction").await;
    seed_assistant_message(&node, &second_receipt, 4, "Wake B response.").await;
    terminalize_request(&node, &second_receipt, "completed").await;
    seed_assistant_message(&node, &first_receipt, 5, "Wake A continues.").await;
    manager
        .observe_session_tick(
            "session-1",
            "1970-01-01T00:00:00Z",
            &mut after,
            &mut delivery_after,
            &sender,
            &engine,
        )
        .await
        .unwrap();
    let updates = parse_buffered_updates(&buffer).await;
    let text: Vec<_> = updates
        .iter()
        .filter_map(|event| {
            event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        })
        .collect();
    assert!(text.contains(&"Wake A continues."));
    assert!(!text.contains(&"Root task finished."));
    assert!(!text.contains(&"Wake task finished."));
    assert!(!text.contains(&"Wake B response."));

    manager
        .handle_cancel(parse_cancel_notification(&json!({"sessionId": "session-1"})).unwrap())
        .await
        .unwrap();
    let interrupted = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }})
        {{ interrupt_requested_at }} }}"#,
            escape_graphql_string(&first)
        ))
        .await;
    ensure_no_errors(&interrupted, "read autonomous interrupt").unwrap();
    assert!(interrupted
        .data
        .as_ref()
        .and_then(|data| data.pointer("/AgentRequest/0/interrupt_requested_at"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    terminalize_request(&node, &first_receipt, "interrupted").await;
    for _ in 0..3 {
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
    }
    let updates = parse_buffered_updates(&buffer).await;
    let first_complete = updates
        .iter()
        .position(|event| {
            event
                .pointer("/params/update/prompt_id")
                .and_then(Value::as_str)
                == Some(format!("notifications-{first}").as_str())
        })
        .unwrap();
    let second_start = updates
        .iter()
        .position(|event| {
            event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                == Some("Wake B response.")
        })
        .unwrap();
    for notice in ["Root task finished.", "Wake task finished."] {
        let positions: Vec<_> = updates
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                (event
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
                    == Some(notice))
                .then_some(index)
            })
            .collect();
        assert_eq!(
            positions.len(),
            1,
            "each deferred notice survives the message high-water and is delivered once"
        );
        assert!(first_complete < positions[0] && positions[0] < second_start);
    }
    assert!(manager.autonomous_delivery.lock().await.is_empty());
}
