use super::*;

async fn test_service_with_graphql(node: Arc<EmbeddedNode>, graphql: String) -> AcpService {
    let identity_dir = tempfile::tempdir().expect("identity tempdir");
    let identity = gents::KeyIdentity::load_or_create(identity_dir.path().join("agent.key"), None)
        .expect("test signing identity");
    let node_did = gents::NodeIdentity::did(&identity).to_string();
    let actor = ::identity::Did::new(node_did.clone()).expect("fixture creator DID");
    let agent_id = gents::default_agent_id_for_node(&node_did);
    let config = AcpServiceConfig {
        node,
        node_did: Arc::from(node_did.as_str()),
        agent_id: Arc::from(agent_id.as_str()),
        current_model: bound_model(),
        grok_home: None,
        plugin_home: std::path::PathBuf::new(),
    };
    gents::store::schema::ensure_runtime_schemas(config.node.as_ref())
        .await
        .expect("runtime schemas");
    crate::commands::grok_shim::seed_test_agent_configuration(
        &config.node,
        &node_did,
        &agent_id,
        &agent_id,
        &config.current_model.model_id,
        true,
    )
    .await;
    let turns = Arc::new(TurnManager::new(
        config.node.clone(),
        crate::commands::grok_shim::turn::TurnManagerConfig {
            actor,
            node_did: config.node_did.to_string(),
            agent_id: config.agent_id.to_string(),
            graphql,
        },
    ));
    let projections = Arc::new(ProjectionEngine::new(
        config.node.clone(),
        crate::commands::grok_shim::projection::BoundModelContext::new(
            config.current_model.model_id.clone(),
            config.current_model.name.clone(),
            config.current_model.total_context_tokens,
        ),
    ));
    AcpService::new(config, turns, projections)
}

#[tokio::test]
async fn delegate_streams_updates_live_before_the_response() {
    let (_staging, config) = config().await;
    gents::store::schema::ensure_runtime_schemas(config.node.as_ref())
        .await
        .expect("runtime schemas");
    let graphql = spawn_mock_graphql(config.node.clone()).await;
    let node = config.node.clone();
    let service = test_service_with_graphql(node, graphql).await;
    let node_did = service.config.node_did.to_string();

    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-live" } }),
        ))
        .await;

    let payload = request_payload(
        "session/prompt",
        json!({
            "sessionId": "s-live",
            "prompt": [{"type": "text", "text": "hello"}],
            "_meta": {"promptId": "prompt-live"},
        }),
    );

    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::commands::grok_shim::protocol::ServerEnvelope,
    >();
    let outbound = AcpOutbound::for_frames(frames_tx);

    let node_for_seed = config.node.clone();
    let seed_handle = tokio::spawn(async move {
        loop {
            let query = r#"{ AgentRequest(filter: { lifecycle_state: { _eq: "pending" } }, limit: 2) { _docID request_id node_did requester_did session_id } }"#;
            let response = node_for_seed.execute(query).await;
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(row) = rows.first() {
                assert_eq!(rows.len(), 1, "live request identity must be exact");
                let request: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(row.clone()).unwrap();
                let escaped_request = gents::graphql::escape_graphql_string(&request.request_id);
                let message_key =
                    gents::session::sequence_message_key(&node_did, "s-live", Some(&node_did), 1);
                let message_doc_id = seed_canonical_assistant_message(
                    node_for_seed.as_ref(),
                    &request,
                    &message_key,
                    1,
                    "",
                    "live answer",
                )
                .await;
                let terminal_output = gents_protocol::graphql::graphql_input_literal(
                    &serde_json::to_value(gents_protocol::output::TerminalOutput::Message {
                        message_doc_id,
                    })
                    .unwrap(),
                )
                .unwrap();
                let mutation = format!(
                    r#"mutation {{
                            update_AgentRequest(
                                filter: {{ request_id: {{ _eq: "{escaped_request}" }} }},
                                input: {{ lifecycle_state: "completed", terminal_output: {terminal_output} }}
                            ) {{ _docID }}
                        }}"#
                );
                let response = node_for_seed.execute(&mutation).await;
                gents::graphql::ensure_no_errors(&response, "terminalize").expect("terminalize");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });

    let service = Arc::new(service);
    let delegate_service = service.clone();
    let handle = tokio::spawn(async move {
        delegate_service
            .handle_acp(&payload, outbound)
            .await
            .expect("delegate should succeed")
    });

    let mut frames: Vec<Value> = Vec::new();
    let response_seen = loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(30), frames_rx.recv())
            .await
            .expect("frame should arrive within timeout")
            .expect("outbound stays open until the delegate resolves");
        let crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload } = frame else {
            continue;
        };
        let value: Value = serde_json::from_str(&payload).expect("acp payload is JSON");
        if value.get("id").is_some() {
            frames.push(value);
            break true;
        }
        frames.push(value);
    };
    assert!(response_seen, "the deferred response must arrive");
    handle.await.expect("delegate task");
    seed_handle.await.expect("seed task");

    let kinds: Vec<String> = frames
        .iter()
        .filter_map(|frame| {
            frame["params"]["update"]["sessionUpdate"]
                .as_str()
                .map(ToOwned::to_owned)
        })
        .collect();
    let echo_index = kinds
        .iter()
        .position(|kind| kind == "user_message_chunk")
        .expect("the user echo must stream live");
    let answer_index = kinds
        .iter()
        .position(|kind| kind == "agent_message_chunk")
        .expect("the assistant answer must stream live");
    assert!(
        echo_index < answer_index,
        "the echo must precede the answer, got {kinds:?}"
    );
    let last = frames.last().expect("response frame");
    assert_eq!(last["id"], 1, "the deferred response closes the stream");
    assert_eq!(last["result"]["stopReason"], "end_turn");
    for frame in &frames[..frames.len() - 1] {
        assert_eq!(frame["params"]["_meta"]["promptId"], "prompt-live");
    }
}

#[tokio::test]
async fn session_new_streams_exact_mcp_completion_after_the_live_response() {
    let (_staging, service) = test_service().await;
    let payload = request_payload(
        "session/new",
        json!({
            "cwd": "/tmp",
            "mcpServers": [],
            "_meta": { "sessionId": "s-mcp-live" },
        }),
    );
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::commands::grok_shim::protocol::ServerEnvelope,
    >();

    service
        .handle_acp(&payload, AcpOutbound::for_frames(frames_tx))
        .await
        .expect("session/new dispatch");

    let first = frames_rx.recv().await.expect("session/new response frame");
    let crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload } = first else {
        panic!("session/new emitted a non-ACP frame first");
    };
    let response = parse_response(&payload);
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["sessionId"], "s-mcp-live");

    let second = frames_rx.recv().await.expect("MCP completion frame");
    let crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload } = second else {
        panic!("session/new emitted a non-ACP completion frame");
    };
    assert_eq!(
        parse_response(&payload),
        json!({
            "jsonrpc": "2.0",
            "method": "_x.ai/mcp_initialized",
            "params": {
                "sessionId": "s-mcp-live",
                "mcpToolCount": 0,
                "elapsedMs": 0,
            },
        })
    );

    assert!(
        frames_rx.try_recv().is_err(),
        "session/new must emit exactly one notification and one response"
    );
}

#[tokio::test]
async fn set_mode_streams_its_notification_before_the_response_on_the_live_path() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-mode-live" } }),
        ))
        .await;

    let payload = request_payload(
        "session/set_mode",
        json!({ "sessionId": "s-mode-live", "modeId": "yolo" }),
    );

    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::commands::grok_shim::protocol::ServerEnvelope,
    >();
    let outbound = AcpOutbound::for_frames(frames_tx);
    service
        .handle_acp(&payload, outbound)
        .await
        .expect("set_mode dispatch");

    let mut saw_mode_update = false;
    let mut saw_response = false;
    while let Ok(frame) =
        tokio::time::timeout(std::time::Duration::from_secs(10), frames_rx.recv()).await
    {
        let Some(crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload }) = frame
        else {
            continue;
        };
        let value: Value = serde_json::from_str(&payload).expect("acp payload is JSON");
        if value.get("id").is_some() {
            saw_response = true;
            break;
        }
        assert!(!saw_response, "a notification arrived after the response");
        assert_eq!(
            value["params"]["update"]["sessionUpdate"], "current_mode_update",
            "the streamed notification is the mode update"
        );
        assert_eq!(
            value["params"]["_meta"]["eventId"], "s-mode-live-1",
            "the mode update consumes the session's first event id"
        );
        saw_mode_update = true;
    }
    assert!(saw_mode_update, "the mode update streamed live");
    assert!(saw_response, "the set_mode response arrived");
    assert!(
        service
            .projections
            .sequencer_arc()
            .event_counter("s-mode-live")
            == 1,
        "exactly one event id was consumed"
    );
}

#[tokio::test]
async fn set_mode_over_a_closed_live_outbound_fails_without_committing_mode_or_event_id() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "s-mode-closed" },
            }),
        ))
        .await;

    let payload = request_payload(
        "session/set_mode",
        json!({ "sessionId": "s-mode-closed", "modeId": "yolo" }),
    );

    let (frames_tx, frames_rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::commands::grok_shim::protocol::ServerEnvelope,
    >();
    let outbound = AcpOutbound::for_frames(frames_tx);
    drop(frames_rx);

    let dispatch_result = service.handle_acp(&payload, outbound).await;
    let send_error =
        dispatch_result.expect_err("the closed outbound must fail the set_mode dispatch");
    assert!(
        send_error.to_string().contains("connection is closed"),
        "the dispatch must surface the closed-connection send error: {send_error}"
    );

    let sessions = service.sessions.lock().await;
    let state = sessions
        .get("s-mode-closed")
        .expect("the session state must exist after session/new");
    assert_eq!(
        state.mode_id, "default",
        "a failed live send must leave the recorded mode untouched"
    );
    drop(sessions);

    assert_eq!(
        service
            .projections
            .sequencer_arc()
            .event_counter("s-mode-closed"),
        0,
        "a failed live send must consume no event id"
    );

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sender = PromptSender::Buffer {
        buffer: buffer.clone(),
    };
    sender
        .send_user_message_chunk(
            service.projections.session_updates(),
            "s-mode-closed",
            "prompt-1",
            &PromptBlock {
                kind: "text".to_string(),
                text: "hello".to_string(),
                meta: None,
            },
            0,
        )
        .await
        .expect("the recovery send must succeed");
    let lines = buffer.lock().await;
    let echo: Value = serde_json::from_str(&lines[0]).expect("echo line");
    assert_eq!(
        echo["params"]["_meta"]["eventId"], "s-mode-closed-1",
        "the next successful send must receive the session's first id"
    );
}
#[tokio::test]
async fn two_sessions_dispatch_through_the_production_entry_with_independent_event_ids() {
    let (_staging, service) = test_service().await;
    let service = Arc::new(service);
    for session in ["s-concurrent-a", "s-concurrent-b"] {
        service . handle_acp_payload (& request_payload ("session/new" , json ! ({ "cwd" : "/tmp" , "mcpServers" : [] , "_meta" : { "sessionId" : session } }) ,)) . await ;
    }
    let service_a = service.clone();
    let service_b = service.clone();
    let dispatch_a = tokio::spawn(async move {
        service_a
            .handle_acp_payload(&request_payload(
                "session/set_mode",
                json ! ({ "sessionId" : "s-concurrent-a" , "modeId" : "yolo" }),
            ))
            .await
    });
    let dispatch_b = tokio::spawn(async move {
        service_b
            .handle_acp_payload(&request_payload(
                "session/set_mode",
                json ! ({ "sessionId" : "s-concurrent-b" , "modeId" : "default" }),
            ))
            .await
    });
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        (dispatch_a.await, dispatch_b.await)
    })
    .await
    .expect("the two buffered session dispatches must both complete");
    let a = a.expect("join a").notifications;
    let b = b.expect("join b").notifications;
    assert_eq!(a.len(), 1, "session A streamed one mode update");
    assert_eq!(b.len(), 1, "session B streamed one mode update");
    let event_id = |line: &str| {
        parse_response(line)["params"]["_meta"]["eventId"]
            .as_str()
            .expect("eventId")
            .to_string()
    };
    assert_eq!(event_id(&a[0]), "s-concurrent-a-1");
    assert_eq!(event_id(&b[0]), "s-concurrent-b-1");
    let sequencer = service.projections.sequencer_arc();
    assert_eq!(sequencer.event_counter("s-concurrent-a"), 1);
    assert_eq!(sequencer.event_counter("s-concurrent-b"), 1);
}
