use super::*;

#[tokio::test]
async fn outbound_close_after_submission_interrupts_and_drains() {
    let (_tempdir, node, node_did) = test_node().await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let manager = Arc::new(TurnManager::new(
        node.clone(),
        test_config(graphql, &node_did),
    ));
    let engine = test_engine(node.clone());
    let (frames_tx, frames_rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::commands::grok_shim::protocol::ServerEnvelope,
    >();
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

    let mut frames_rx = frames_rx;
    let outbound_closed = Arc::new(tokio::sync::Notify::new());
    let closed_signal = outbound_closed.clone();
    let closer = tokio::spawn(async move {
        let _ = frames_rx.recv().await;
        drop(frames_rx);
        closed_signal.notify_one();
    });
    let node_for_seed = node.clone();
    let did_for_seed = node_did.clone();
    let seed_handle = tokio::spawn(async move {
        let request_id = wait_for_pending_request(&node_for_seed, &did_for_seed).await;
        outbound_closed.notified().await;
        let _tool_doc = seed_tool_call(
            &node_for_seed,
            &request_id,
            "call-1",
            "read_file",
            "running",
            "",
        )
        .await;
    });

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        manager.handle_prompt(prompt, &sender, &engine),
    )
    .await
    .expect("prompt should resolve within timeout");
    assert!(result.is_err(), "a closed outbound must fail the turn");
    closer.await.expect("closer task");
    seed_handle.await.expect("seed task");

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
        "the submitted request must be interrupted after the mid-turn send failure"
    );

    let second = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("again")],
            "_meta": {"promptId": "prompt-2"},
        }),
        Some(json!(2)),
    )
    .unwrap();
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
            for row in &rows {
                let receipt: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(row.clone()).unwrap();
                terminalize_request(&node_for_terminalize, &receipt, "interrupted").await;
            }
            if rows.is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    });
    let (buffer2, sender2) = buffer_sender();
    let second_result = tokio::time::timeout(
        Duration::from_secs(30),
        manager.handle_prompt(second, &sender2, &engine),
    )
    .await
    .expect("second prompt should resolve within timeout")
    .expect("second prompt must be accepted after the drain");
    drop(buffer2);
    terminalize_handle.abort();
    assert_eq!(second_result["stopReason"], json!("cancelled"));
}

#[tokio::test]
async fn cancel_before_request_id_resolves_cancelled_and_permits_reuse() {
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

    let cancel = parse_cancel_notification(&json!({
        "sessionId": "session-1",
        "_meta": {"cancelSubagents": true, "promptId": "prompt-1"},
    }))
    .unwrap();
    manager
        .handle_cancel(cancel)
        .await
        .expect("cancel should succeed");

    submission_release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt task")
        .expect("cancel-before-id must resolve cancelled, not error");

    assert_eq!(result["stopReason"], json!("cancelled"));

    let second = parse_prompt_request(
        &json!({
            "sessionId": "session-1",
            "prompt": [text_block("again")],
            "_meta": {"promptId": "prompt-2"},
        }),
        Some(json!(2)),
    )
    .unwrap();
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
            for row in &rows {
                let receipt: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(row.clone()).unwrap();
                terminalize_request(&node_for_terminalize, &receipt, "interrupted").await;
            }
            if rows.is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        manager.handle_prompt(second, &sender, &engine),
    )
    .await
    .expect("second prompt should resolve within timeout")
    .expect("second prompt should succeed");
    assert_eq!(result["stopReason"], json!("cancelled"));
    terminalize_handle.abort();
}

#[tokio::test]
async fn disconnect_before_request_id_resolves_cancelled() {
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

    manager
        .handle_disconnect()
        .await
        .expect("disconnect should succeed");

    submission_release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt task")
        .expect("disconnect-before-id must resolve cancelled, not error");
    assert_eq!(result["stopReason"], json!("cancelled"));
}
