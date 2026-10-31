use super::*;

#[tokio::test]
async fn disconnect_parked_before_latch_with_finishing_submission_failure_resolves_cancelled() {
    let (_tempdir, node, node_did) = test_node().await;
    let submission_arrived = Arc::new(tokio::sync::Notify::new());
    let submission_release = Arc::new(tokio::sync::Notify::new());
    let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let submission_fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let graphql = spawn_gated_mock_graphql_with_failure(
        node.clone(),
        Some((
            submission_arrived.clone(),
            submission_release.clone(),
            gate_armed,
        )),
        Some(submission_fail),
    )
    .await;
    let manager = Arc::new(TurnManager::new(
        node.clone(),
        test_config(graphql, &node_did),
    ));
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

    let prompt_handle = tokio::spawn({
        let manager = manager.clone();
        let sender = sender.clone();
        let engine = engine.clone();
        async move { manager.handle_prompt(prompt, &sender, &engine).await }
    });

    tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
        .await
        .expect("submission should arrive at the gated endpoint");

    let gate = TestGate {
        arrived: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    *manager.disconnect_gate.lock().await = Some(gate.clone());
    let manager_for_disconnect = manager.clone();
    let disconnect_handle = tokio::spawn(async move {
        manager_for_disconnect
            .handle_disconnect()
            .await
            .expect("disconnect should succeed")
    });

    tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
        .await
        .expect("disconnect should park at the seam");

    {
        let state = manager.state.lock().await;
        assert!(state.closed, "the disconnect has published closed");
        assert!(
            state.entries.is_empty(),
            "the disconnect has drained the entry table"
        );
    }

    submission_release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt task")
        .expect("submission failure in the closed-drained window must resolve cancelled");
    assert_eq!(
        result["stopReason"],
        json!("cancelled"),
        "the disconnect's required stopReason wins over the GraphQL error"
    );

    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), disconnect_handle)
        .await
        .expect("disconnect should resolve within timeout")
        .expect("disconnect task");

    {
        let state = manager.state.lock().await;
        assert!(state.entries.is_empty());
        assert!(state.closed);
    }

    let query = r#"{ AgentRequest { request_id } }"#;
    let response = node.execute(query).await;
    ensure_no_errors(&response, "test request query").expect("query");
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.is_empty(),
        "a submission failed into a closed connection must not mint an AgentRequest, got {rows:?}"
    );
}

#[tokio::test]
async fn cancel_parked_before_latch_with_finishing_submission_failure_resolves_cancelled() {
    let (_tempdir, node, node_did) = test_node().await;
    let submission_arrived = Arc::new(tokio::sync::Notify::new());
    let submission_release = Arc::new(tokio::sync::Notify::new());
    let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let submission_fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let graphql = spawn_gated_mock_graphql_with_failure(
        node.clone(),
        Some((
            submission_arrived.clone(),
            submission_release.clone(),
            gate_armed,
        )),
        Some(submission_fail),
    )
    .await;
    let manager = Arc::new(TurnManager::new(
        node.clone(),
        test_config(graphql, &node_did),
    ));
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

    let prompt_handle = tokio::spawn({
        let manager = manager.clone();
        let sender = sender.clone();
        let engine = engine.clone();
        async move { manager.handle_prompt(prompt, &sender, &engine).await }
    });

    tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
        .await
        .expect("submission should arrive at the gated endpoint");

    let gate = TestGate {
        arrived: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    *manager.cancel_drain_gate.lock().await = Some(gate.clone());
    let cancel = parse_cancel_notification(&json!({
        "sessionId": "session-1",
        "_meta": {"cancelSubagents": true, "promptId": "prompt-1"},
    }))
    .unwrap();
    let manager_for_cancel = manager.clone();
    let cancel_handle = tokio::spawn(async move {
        manager_for_cancel
            .handle_cancel(cancel)
            .await
            .expect("cancel should succeed")
    });

    tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
        .await
        .expect("cancel should park at the seam");

    {
        let state = manager.state.lock().await;
        assert!(
            !state.closed,
            "an explicit cancel leaves the connection open"
        );
        assert!(
            state.entries.is_empty(),
            "the cancel has removed the pending entry"
        );
    }

    submission_release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt task")
        .expect("submission failure in the cancel-drain window must resolve cancelled");
    assert_eq!(
        result["stopReason"],
        json!("cancelled"),
        "the explicit cancel's required stopReason wins over the GraphQL error"
    );

    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), cancel_handle)
        .await
        .expect("cancel should resolve within timeout")
        .expect("cancel task");

    {
        let state = manager.state.lock().await;
        assert!(state.entries.is_empty());
        assert!(!state.closed);
    }

    let query = r#"{ AgentRequest { request_id } }"#;
    let response = node.execute(query).await;
    ensure_no_errors(&response, "test request query").expect("query");
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.is_empty(),
        "a submission failed into a cancelled turn must not mint an AgentRequest, got {rows:?}"
    );
}
