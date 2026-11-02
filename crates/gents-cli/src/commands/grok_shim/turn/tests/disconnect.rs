use super::*;

#[tokio::test]
async fn prompt_gated_before_insertion_is_rejected_after_disconnect() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
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

    let gate = TestGate {
        arrived: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    *manager.insertion_gate.lock().await = Some(gate.clone());

    let manager_for_prompt = manager.clone();
    let sender_for_prompt = sender.clone();
    let engine_for_prompt = engine.clone();
    let prompt_handle = tokio::spawn(async move {
        manager_for_prompt
            .handle_prompt(prompt, &sender_for_prompt, &engine_for_prompt)
            .await
    });

    tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
        .await
        .expect("prompt should arrive at the insertion gate");

    manager
        .handle_disconnect()
        .await
        .expect("disconnect should succeed");

    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt task");
    let error = result.expect_err("a prompt gated across disconnect must be rejected");
    assert!(
        error.to_string().contains("already disconnected"),
        "rejection must name the closed connection, got: {error:#}"
    );

    {
        let state = manager.state.lock().await;
        assert!(
            state.entries.is_empty(),
            "no entry may be inserted after the closed latch"
        );
        assert!(state.closed, "the connection stays closed");
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
        "a prompt rejected by the closed latch must never create an AgentRequest, got {rows:?}"
    );

    manager
        .handle_disconnect()
        .await
        .expect("duplicate disconnect should succeed");
    {
        let state = manager.state.lock().await;
        assert!(state.entries.is_empty());
        assert!(state.closed);
    }
}

#[tokio::test]
async fn disconnect_drains_an_already_pending_entry_and_duplicate_disconnect_is_a_noop() {
    let (_tempdir, node, node_did) = test_node().await;
    let submission_arrived = Arc::new(tokio::sync::Notify::new());
    let submission_release = Arc::new(tokio::sync::Notify::new());
    let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let graphql = spawn_gated_mock_graphql(
        node.clone(),
        Some((
            submission_arrived.clone(),
            submission_release.clone(),
            gate_armed,
        )),
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

    {
        let state = manager.state.lock().await;
        assert_eq!(state.entries.len(), 1, "the prompt entry must be pending");
    }
    manager
        .handle_disconnect()
        .await
        .expect("disconnect should succeed");
    {
        let state = manager.state.lock().await;
        assert!(
            state.entries.is_empty(),
            "disconnect must drain every entry"
        );
        assert!(state.closed);
    }

    manager
        .handle_disconnect()
        .await
        .expect("duplicate disconnect should succeed");
    {
        let state = manager.state.lock().await;
        assert!(state.entries.is_empty());
        assert!(state.closed);
    }

    submission_release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt task")
        .expect("the drained prompt must resolve cancelled, not error");
    assert_eq!(result["stopReason"], json!("cancelled"));
}

#[tokio::test]
async fn send_failure_after_submission_interrupts_the_request() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
    let engine = test_engine(node.clone());
    let (frames_tx, frames_rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::commands::grok_shim::protocol::ServerEnvelope,
    >();
    drop(frames_rx);
    let sender = PromptSender::Live {
        outbound: AcpOutbound::for_frames(frames_tx),
    };

    let prompt = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("hello")],
            "_meta": {"promptId": "prompt-1"},
        }),
        Some(json!(1)),
    )
    .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        manager.handle_prompt(prompt, &sender, &engine),
    )
    .await
    .expect("prompt should resolve within timeout");
    assert!(
        result.is_err(),
        "closed outbound must surface a send failure"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    let query = r#"{ AgentRequest { request_id interrupt_requested_at } }"#;
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
        rows.iter().any(|row| row
            .get("interrupt_requested_at")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())),
        "the submitted request must be interrupted after the send failure"
    );
}

#[tokio::test]
async fn second_prompt_for_live_session_is_rejected() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = Arc::new(TurnManager::new(
        node.clone(),
        test_config(graphql, &node_did),
    ));
    let projections = test_engine(node.clone());
    let (buffer, sender) = buffer_sender();

    let first = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("first")],
            "_meta": {"promptId": "prompt-1"},
        }),
        Some(json!(1)),
    )
    .unwrap();
    let manager_for_first = manager.clone();
    let sender_for_first = sender.clone();
    let engine_for_first = projections.clone();
    let first_handle = tokio::spawn(async move {
        manager_for_first
            .handle_prompt(first, &sender_for_first, &engine_for_first)
            .await
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if !buffer.lock().await.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "first prompt never echoed; pending entry never went live"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let second = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("second")],
            "_meta": {"promptId": "prompt-2"},
        }),
        Some(json!(2)),
    )
    .unwrap();
    let rejection = manager
        .handle_prompt(second, &sender, &projections)
        .await
        .expect_err("second prompt for a live session must be rejected");
    assert!(
        rejection.to_string().contains("live prompt"),
        "rejection must name the one-pending-per-session rule, got: {rejection}"
    );

    let node_for_terminalize = node.clone();
    let did_for_terminalize = node_did.clone();
    let terminalize_handle = tokio::spawn(async move {
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
    let first_result = tokio::time::timeout(Duration::from_secs(30), first_handle)
        .await
        .expect("first prompt should resolve within timeout")
        .expect("first prompt task")
        .expect("first prompt should succeed");
    assert_eq!(first_result["stopReason"], json!("end_turn"));
    terminalize_handle.await.expect("terminalize task");
}
