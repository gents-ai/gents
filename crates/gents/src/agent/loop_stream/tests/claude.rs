/// Two Messages turns through the owned loop on SSE fixtures: two signed
/// thinking blocks (one empty) and `tool_use echo` with streamed arguments →
/// gents executes echo → signed-thinking/tool-result continuation → text
/// `done`. The streamed native arguments are asserted through the
/// canonical admission binding (`load_accepted_for_dispatch`) and exact
/// reconstruction of its accepted `ToolArguments` payload — canonical
/// admission resolves a terminal (completed) call too. The exact delivered
/// output is asserted through `session::load_tool_call_result` (defect C1,
/// live-confirmed by write request #8).
#[tokio::test]
async fn claude_messages_tool_round_trip_through_owned_loop() {
    for scope_kind in [
        gents_protocol::rendered_request::CaptureScopeKind::Inference,
        gents_protocol::rendered_request::CaptureScopeKind::OneShot,
    ] {
        signed_claude_tool_round_trip_with_scope(scope_kind).await;
    }
}

async fn signed_claude_tool_round_trip_with_scope(
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) {
    use crate::claude_messages::{
        install_messages_sse_fixtures, lock_fixtures_for_test, sse_fixture_final_text,
    };
    use crate::claude_subscription::{ClaudeSubscriptionClient, StaticBearer};
    use rig::client::CompletionClient;

    let first_thinking = "reasoned";
    let first_signature = "sig-first";
    let second_thinking = "";
    let second_signature = "sig-empty";
    let first_turn = [
        serde_json::json!({"type":"content_block_start","index":0,
            "content_block":{"type":"thinking","thinking":first_thinking}}),
        serde_json::json!({"type":"content_block_delta","index":0,
            "delta":{"type":"signature_delta","signature":first_signature}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"content_block_start","index":1,
            "content_block":{"type":"thinking","thinking":second_thinking}}),
        serde_json::json!({"type":"content_block_delta","index":1,
            "delta":{"type":"signature_delta","signature":second_signature}}),
        serde_json::json!({"type":"content_block_stop","index":1}),
        serde_json::json!({"type":"content_block_start","index":2,
            "content_block":{"type":"tool_use","id":"toolu_1","name":"echo","input":{}}}),
        serde_json::json!({"type":"content_block_delta","index":2,
            "delta":{"type":"input_json_delta","partial_json":"{\"text\":\"hi\"}"}}),
        serde_json::json!({"type":"content_block_stop","index":2}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
            "usage":{"input_tokens":10,"output_tokens":5}}),
        serde_json::json!({"type":"message_stop"}),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();
    let _guard = lock_fixtures_for_test();
    install_messages_sse_fixtures(vec![first_turn, sse_fixture_final_text("done")]);
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;

    // Fixture-only: a refusing bearer keeps an extra turn off the network.
    let model =
        ClaudeSubscriptionClient::with_bearer(Arc::new(StaticBearer::failing("no credential")))
            .completion_model("claude-sonnet-5");
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![echo_tool()]);
    let mut config = owned_config(4);
    config.on_rendered_request = Some(crate::rendered_request::scope::ambient_arming_sink(
        scope_kind,
    ));
    config.provider_input_counter = Arc::new(crate::provider_input::ProviderInputCounter::new(
        crate::BackendProviderKind::ClaudeCliSubscription,
        crate::OpenAiWireApi::ChatCompletions,
        "claude-sonnet-5",
    ));
    config.replay = crate::provider_input::replay::owned_replay_input(
        node.clone(),
        lifecycle.request().clone(),
        lifecycle
            .request_commit_cid()
            .expect("claimed request commit CID")
            .to_owned(),
        scope_kind,
    );
    let request_commit_cid = lifecycle
        .request_commit_cid()
        .expect("claimed request commit CID")
        .to_owned();
    let capture_factory =
        crate::rendered_request::defra_rendered_request_capture_factory(node.clone());
    let capture_scope = crate::rendered_request::scope_from_factory(
        crate::rendered_request::context_for_claimed_request(
            lifecycle.request(),
            &request_commit_cid,
            "claude-sonnet-5".to_owned(),
        ),
        Some(&capture_factory),
    )
    .expect("DefraDB rendered-request capture scope");

    let stream = run_loop_stream(
        model,
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("use the echo tool")),
        Vec::new(),
        tools,
        config,
    );
    let collected = collect_owned_scripted_stream_with_capture_scope(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::ClaudeMessages,
        Some(capture_scope),
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);

    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

    // The second provider send is captured by the real DefraDB sink. Its
    // signed blocks can appear only after the accepted first turn's physical
    // header, closure, and Claude capture pass the replay resolver.
    let request_doc_id = lifecycle.request().doc_id.clone();
    let captures = node
        .execute(&format!(
            r#"{{ RenderedRequest(filter: {{ request_doc_id: {{ _eq: "{}" }} }}, order: {{ turn_index: ASC }}) {{ turn_index capture_scope source request_commit_cid capture_version request_json }} }}"#,
            crate::graphql::escape_graphql_string(&request_doc_id),
        ))
        .await;
    assert!(
        !captures.has_errors(),
        "RenderedRequest query failed: {:?}",
        captures.errors
    );
    let capture_rows = captures.data.as_ref().expect("capture data")["RenderedRequest"]
        .as_array()
        .expect("capture rows");
    assert_eq!(capture_rows.len(), 2, "both Claude sends must be durable");
    let expected_source = serde_json::to_value(
        gents_protocol::rendered_request::RenderedRequestSource::ClaudeCliSubscription,
    )
    .expect("Claude capture source");
    for (turn_index, row) in capture_rows.iter().enumerate() {
        assert_eq!(row["capture_scope"], format!("{scope_kind}.1"));
        assert_eq!(
            row["turn_index"].as_u64(),
            Some(turn_index as u64),
            "capture turn coordinate"
        );
        assert_eq!(row["source"], expected_source, "Claude origin is physical");
        assert_eq!(
            row["request_commit_cid"].as_str(),
            Some(request_commit_cid.as_str())
        );
    }
    let second = &capture_rows[1];
    let (captured_body, _) = crate::rendered_request::decode_capture_pair(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        u32::try_from(second["capture_version"].as_u64().expect("capture version"))
            .expect("version fits u32"),
        second["request_json"]
            .as_str()
            .expect("encoded request JSON"),
    )
    .await
    .expect("decode the second physical Claude capture");
    let messages = captured_body["messages"].as_array().expect("Messages body");
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("accepted assistant replay in second send");
    let blocks = assistant["content"].as_array().expect("assistant blocks");
    let thinking = blocks
        .iter()
        .filter(|block| block["type"] == "thinking")
        .map(|block| {
            serde_json::json!({
                "thinking": block["thinking"], "signature": block["signature"]
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        thinking,
        vec![
            serde_json::json!({"thinking": first_thinking, "signature": first_signature}),
            serde_json::json!({"thinking": second_thinking, "signature": second_signature}),
        ],
        "the continuation must replay both exact signed blocks, including empty text"
    );
    assert!(
        messages.iter().any(|message| message["role"] == "user"
            && message["content"]
                .as_array()
                .is_some_and(|content| content.iter().any(|block| {
                    block["type"] == "tool_result" && block["tool_use_id"] == "toolu_1"
                }))),
        "second send must include the accepted tool result"
    );

    // The physical AgentToolCall row is the only handle; address it by
    // `_docID` within its session scope.
    let resp = node
        .execute("query { AgentToolCall { _docID tool_name lifecycle_state } }")
        .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let echo = rows
        .iter()
        .find(|row| row.get("tool_name").and_then(|value| value.as_str()) == Some("echo"))
        .unwrap_or_else(|| panic!("expected an echo AgentToolCall; rows: {rows:?}"));
    assert_eq!(echo["lifecycle_state"], "completed");
    let tool_doc_id = echo["_docID"].as_str().expect("_docID").to_string();
    let session_id = lifecycle.request().session_id.clone();

    // Canonical admission works terminal too: the completed row still resolves
    // its immutable accepted binding.
    let accepted = crate::tool_call_lifecycle::ToolCallLifecycle::load_accepted_for_dispatch(
        &node,
        &tool_doc_id,
        "did:test:test",
        &session_id,
        None,
    )
    .await
    .expect("canonical admission binding for the completed echo call");
    assert_eq!(accepted.tool_name, "echo");
    assert_eq!(
        accepted.request_doc_id, request_doc_id,
        "accepted binding must not cross physical request identity"
    );

    // The accepted arguments stream is native JSON exactly as the fixture
    // streamed it.
    let arguments = crate::session::load_canonical_payload_from_node(
        &node,
        &accepted.request_doc_id,
        "did:test:test",
        None,
        &accepted.arguments,
    )
    .await
    .expect("accepted arguments stream");
    assert!(matches!(
        arguments.declaration.payload,
        gents_protocol::output::StreamPayload::ToolArguments { ref name, .. } if name == "echo"
    ));
    let args: serde_json::Value = serde_json::from_str(&arguments.text).expect("native JSON args");
    assert_eq!(
        args,
        serde_json::json!({"text": "hi"}),
        "streamed arguments must persist exactly as emitted"
    );

    // The delivered result reconstructs exactly through the session owner.
    let native = crate::session::load_tool_call_result(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &tool_doc_id,
        "did:test:test",
        &session_id,
        None,
    )
    .await
    .expect("delivered tool result");
    assert_eq!(
        crate::tool_call_lifecycle::query::render_tool_result(&native).unwrap(),
        "ECHOED",
    );
}
