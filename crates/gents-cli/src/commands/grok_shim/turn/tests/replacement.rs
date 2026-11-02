use super::*;

#[tokio::test]
async fn cancelled_submission_cannot_capture_an_exact_key_replacement() {
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

    let prompt = || {
        parse_prompt_request(
            &json!({
                "sessionId": "session-reuse",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "same-prompt"},
            }),
            Some(json!(1)),
        )
        .expect("prompt")
    };
    let first = tokio::spawn({
        let manager = manager.clone();
        let sender = sender.clone();
        let engine = engine.clone();
        let prompt = prompt();
        async move { manager.handle_prompt(prompt, &sender, &engine).await }
    });
    tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
        .await
        .expect("first submission reached gate");

    let cancel = parse_cancel_notification(&json!({
        "sessionId": "session-reuse",
        "_meta": {"promptId": "same-prompt"},
    }))
    .expect("cancel");
    manager.handle_cancel(cancel).await.expect("cancel first");

    let replacement = tokio::spawn({
        let manager = manager.clone();
        let sender = sender.clone();
        let engine = engine.clone();
        let prompt = prompt();
        async move { manager.handle_prompt(prompt, &sender, &engine).await }
    });
    let replacement_request_id = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let request_id = manager
                .state
                .lock()
                .await
                .entries
                .get(&("session-reuse".to_string(), "same-prompt".to_string()))
                .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.clone()));
            if let Some(request_id) = request_id {
                break request_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement request id registered");

    submission_release.notify_one();
    let first_result = tokio::time::timeout(Duration::from_secs(30), first)
        .await
        .expect("old prompt resolves")
        .expect("old prompt task")
        .expect("old prompt result");
    assert_eq!(first_result["stopReason"], json!("cancelled"));
    assert_eq!(
        manager
            .state
            .lock()
            .await
            .entries
            .get(&("session-reuse".to_string(), "same-prompt".to_string()))
            .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.as_str())),
        Some(replacement_request_id.as_str()),
        "the old generation must not overwrite the exact-key replacement"
    );

    let cancel_replacement = parse_cancel_notification(&json!({
        "sessionId": "session-reuse",
        "_meta": {"promptId": "same-prompt"},
    }))
    .expect("cancel replacement");
    manager
        .handle_cancel(cancel_replacement)
        .await
        .expect("cancel replacement");
    let replacement_result = tokio::time::timeout(Duration::from_secs(30), replacement)
        .await
        .expect("replacement resolves")
        .expect("replacement task")
        .expect("replacement result");
    assert_eq!(replacement_result["stopReason"], json!("cancelled"));
}

#[tokio::test]
async fn failed_submission_cannot_remove_an_exact_key_replacement() {
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
    let prompt = || {
        parse_prompt_request(
            &json!({
                "sessionId": "session-failed-reuse",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "same-prompt"},
            }),
            Some(json!(1)),
        )
        .expect("prompt")
    };

    let first = tokio::spawn({
        let manager = manager.clone();
        let sender = sender.clone();
        let engine = engine.clone();
        let prompt = prompt();
        async move { manager.handle_prompt(prompt, &sender, &engine).await }
    });
    tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
        .await
        .expect("first submission reached gate");

    manager
        .handle_cancel(
            parse_cancel_notification(&json!({
                "sessionId": "session-failed-reuse",
                "_meta": {"promptId": "same-prompt"},
            }))
            .expect("cancel first"),
        )
        .await
        .expect("cancel first");

    let replacement = tokio::spawn({
        let manager = manager.clone();
        let sender = sender.clone();
        let engine = engine.clone();
        let prompt = prompt();
        async move { manager.handle_prompt(prompt, &sender, &engine).await }
    });
    let key = (
        "session-failed-reuse".to_string(),
        "same-prompt".to_string(),
    );
    let replacement_request_id = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(request_id) = manager
                .state
                .lock()
                .await
                .entries
                .get(&key)
                .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.clone()))
            {
                break request_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement request id registered");

    submission_release.notify_one();
    let first_result = tokio::time::timeout(Duration::from_secs(30), first)
        .await
        .expect("old prompt resolves")
        .expect("old prompt task")
        .expect("old failed submission resolves as cancelled");
    assert_eq!(first_result["stopReason"], json!("cancelled"));
    assert_eq!(
        manager
            .state
            .lock()
            .await
            .entries
            .get(&key)
            .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.as_str())),
        Some(replacement_request_id.as_str()),
        "the failed old generation must not remove its replacement"
    );

    manager
        .handle_cancel(
            parse_cancel_notification(&json!({
                "sessionId": "session-failed-reuse",
                "_meta": {"promptId": "same-prompt"},
            }))
            .expect("cancel replacement"),
        )
        .await
        .expect("cancel replacement");
    let replacement_result = tokio::time::timeout(Duration::from_secs(30), replacement)
        .await
        .expect("replacement resolves")
        .expect("replacement task")
        .expect("replacement result");
    assert_eq!(replacement_result["stopReason"], json!("cancelled"));
}

#[tokio::test]
async fn cancel_selection_cannot_drain_an_exact_key_replacement() {
    let (_tempdir, node, node_did) = test_node().await;
    let manager = Arc::new(TurnManager::new(
        node,
        test_config("http://127.0.0.1:1/".to_string(), &node_did),
    ));
    let key = ("session-reuse".to_string(), "same-prompt".to_string());
    let old_generation = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
    let (old_tx, mut old_rx) = oneshot::channel::<Result<Value>>();
    manager.state.lock().await.entries.insert(
        key.clone(),
        PendingPrompt {
            response_tx: Some(old_tx),
            request: Some(serde_json::from_value(json!({"request_id": "old-request"})).unwrap()),
            cancel_before_id: old_generation,
            drained: false,
        },
    );

    let gate = TestGate {
        arrived: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    *manager.cancel_selection_gate.lock().await = Some(gate.clone());
    let cancel = parse_cancel_notification(&json!({
        "sessionId": "session-reuse",
        "_meta": {"promptId": "same-prompt"},
    }))
    .expect("cancel");
    let cancel_handle = tokio::spawn({
        let manager = manager.clone();
        async move { manager.handle_cancel(cancel).await }
    });
    tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
        .await
        .expect("cancel should snapshot the old generation");

    let replacement_generation = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
    let (replacement_tx, mut replacement_rx) = oneshot::channel::<Result<Value>>();
    {
        let mut state = manager.state.lock().await;
        let mut old = state.entries.remove(&key).expect("old generation");
        old.resolve(Ok(json!({"stopReason": "end_turn"})));
        state.entries.insert(
            key.clone(),
            PendingPrompt {
                response_tx: Some(replacement_tx),
                request: Some(
                    serde_json::from_value(json!({"request_id": "replacement-request"})).unwrap(),
                ),
                cancel_before_id: replacement_generation.clone(),
                drained: false,
            },
        );
    }
    assert_eq!(
        old_rx
            .try_recv()
            .expect("old generation terminal response")
            .expect("old generation result")["stopReason"],
        json!("end_turn")
    );

    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), cancel_handle)
        .await
        .expect("cancel should finish")
        .expect("cancel task")
        .expect("cancel result");

    {
        let state = manager.state.lock().await;
        let replacement = state.entries.get(&key).expect("replacement survives");
        assert!(Arc::ptr_eq(
            &replacement.cancel_before_id,
            &replacement_generation
        ));
        assert_eq!(
            replacement
                .request
                .as_ref()
                .map(|row| row.request_id.as_str()),
            Some("replacement-request")
        );
        assert!(!replacement.drained);
    }
    assert!(matches!(
        replacement_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    let mut replacement = manager
        .state
        .lock()
        .await
        .entries
        .remove(&key)
        .expect("replacement cleanup");
    replacement.resolve(Ok(json!({"stopReason": "cancelled"})));
    assert_eq!(
        replacement_rx
            .try_recv()
            .expect("replacement cleanup response")
            .expect("replacement cleanup result")["stopReason"],
        json!("cancelled")
    );
}
