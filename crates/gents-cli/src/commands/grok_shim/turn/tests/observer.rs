use super::*;

#[tokio::test]
async fn a_projected_write_wakes_observation_before_the_fallback() {
    let (_tempdir, node, node_did) = test_node().await;
    let mut changes = subscribe_projected_changes(&node);
    assert!(changes.is_some(), "every projected collection is installed");
    let started = tokio::time::Instant::now();
    seed_runtime_wake(&node, &node_did, "wake").await;
    wait_for_projected_change(&mut changes).await;
    assert!(changes.is_some());
    assert!(started.elapsed() < PROJECTION_FALLBACK);
}

#[tokio::test]
async fn session_observer_preserves_handoff_and_delivers_late_wake() {
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
            "sessionId": "session-1", "prompt": [text_block("start")],
            "_meta": {"promptId": "foreground-1"},
        }),
        Some(json!(1)),
    )
    .unwrap();
    let turn = {
        let manager = manager.clone();
        let engine = engine.clone();
        let sender = sender.clone();
        tokio::spawn(async move { manager.handle_prompt(prompt, &sender, &engine).await })
    };
    let root_receipt = wait_for_pending_request(&node, &node_did).await;
    let late_tool_doc =
        seed_tool_call(&node, &root_receipt, "late-bash", "bash", "running", "").await;
    seed_assistant_message(&node, &root_receipt, 1, "Root response.").await;
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
    terminalize_request(&node, &root_receipt, "completed").await;
    let result = tokio::time::timeout(Duration::from_secs(30), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["stopReason"], "end_turn");

    let update = node
        .execute(&format!(
            r#"mutation {{ update_AgentToolCall(
        filter: {{ _docID: {{ _eq: "{}" }} }},
        input: {{ lifecycle_state: "failed" }}) {{ _docID }} }}"#,
            escape_graphql_string(&late_tool_doc)
        ))
        .await;
    ensure_no_errors(&update, "late tool finish").unwrap();
    let wake_receipt =
        seed_runtime_wake(&node, &node_did, "internal notification instruction").await;
    let wake = wake_receipt.request_id.clone();
    seed_assistant_message(&node, &wake_receipt, 2, "Background work finished.").await;
    terminalize_request(&node, &wake_receipt, "completed").await;
    let (pending_tx, _pending_rx) = oneshot::channel();
    let foreground_key = ("session-1".to_owned(), "new-foreground".to_owned());
    manager.state.lock().await.entries.insert(
        foreground_key.clone(),
        PendingPrompt {
            response_tx: Some(pending_tx),
            request: Some(
                serde_json::from_value(json!({"request_id": "new-foreground-request"})).unwrap(),
            ),
            cancel_before_id: Arc::new(Mutex::new(CancelBeforeIdLatch::default())),
            drained: false,
        },
    );
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
    assert!(
        !parse_buffered_updates(&buffer)
            .await
            .iter()
            .any(|event| event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                == Some("Background work finished.")),
        "wake output must remain undelivered while the pager has a foreground prompt"
    );
    assert!(
        parse_buffered_updates(&buffer).await.iter().any(|event| {
            event
                .pointer("/params/update/toolCallId")
                .and_then(Value::as_str)
                == Some("late-bash")
                && event
                    .pointer("/params/update/status")
                    .and_then(Value::as_str)
                    == Some("failed")
        }),
        "background terminal updates must flow while a new foreground is pending"
    );
    manager.state.lock().await.entries.remove(&foreground_key);
    let response_delivery = manager.begin_foreground_delivery("session-1").await;
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
    assert!(!parse_buffered_updates(&buffer).await.iter().any(|event| {
        event
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
            == Some("Background work finished.")
    }));
    drop(response_delivery);
    for _ in 0..2 {
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
    let text: Vec<_> = updates
        .iter()
        .filter_map(|event| {
            event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(
        text.iter()
            .filter(|value| **value == "Root response.")
            .count(),
        1
    );
    assert_eq!(
        text.iter()
            .filter(|value| **value == "Background work finished.")
            .count(),
        1
    );
    assert!(!text.contains(&"internal notification instruction"));
    let failed = updates
        .iter()
        .find(|event| {
            event
                .pointer("/params/update/status")
                .and_then(Value::as_str)
                == Some("failed")
        })
        .unwrap();
    assert!(
        failed.pointer("/params/_meta/promptId").is_none(),
        "late root update must pass the pager's idle prompt gate"
    );
    let completions: Vec<_> = updates
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            event
                .pointer("/params/update/sessionUpdate")
                .and_then(Value::as_str)
                == Some("turn_completed")
        })
        .collect();
    assert_eq!(completions.len(), 1);
    let (completion_index, completion) = completions[0];
    assert_eq!(completion["method"], "_x.ai/session_notification");
    assert_eq!(
        completion["params"]["update"]["prompt_id"],
        format!("notifications-{wake}")
    );
    assert_eq!(
        completion["params"]["_meta"]["promptId"],
        format!("notifications-{wake}")
    );
    assert!(updates[..completion_index].iter().any(|event| {
        event
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
            == Some("Background work finished.")
    }));

    manager
        .observe_session("session-1", sender.clone(), engine.clone())
        .await;
    let delivery = manager.delivery_gate.lock().await;
    let prompt = parse_prompt_request(
        &json!({
            "sessionId": "session-1", "prompt": [text_block("after wake")],
        }),
        None,
    )
    .unwrap();
    let mut admission = Box::pin(manager.handle_prompt(prompt, &sender, &engine));
    tokio::select! {
        biased;
        result = &mut admission => panic!("admission crossed delivery gate: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }
    assert!(manager.state.lock().await.entries.is_empty());
    tokio::time::timeout(Duration::from_secs(5), manager.handle_disconnect())
        .await
        .unwrap()
        .unwrap();
    assert!(manager.observers.lock().await.is_empty());
    drop(delivery);
    assert!(admission
        .await
        .unwrap_err()
        .to_string()
        .contains("disconnected"));
}
