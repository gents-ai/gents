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
    let ClaudeFixtureRun {
        node,
        lifecycle,
        collected,
        request_commit_cid,
    } = run_claude_fixture_turns(first_turn, scope_kind).await;
    assert!(collected.error.is_none(), "{:?}", collected.error);

    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

    let request_doc_id = lifecycle.request().doc_id.clone();
    let sends =
        captured_claude_sends(&node, &request_doc_id, &request_commit_cid, scope_kind).await;
    let captured_body = &sends[1];
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

    let held = crate::store::config_client::ConfigApplyTxn::begin_local(&node, None)
        .await
        .unwrap();
    let accepted = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::tools::call::lifecycle::ToolCallLifecycle::load_accepted_for_dispatch(
            &node,
            &tool_doc_id,
            "did:test:test",
            &session_id,
            None,
        ),
    )
    .await
    .expect("immutable admission lookup must not wait for the mutation gate")
    .expect("canonical admission binding for the completed echo call");
    held.discard().await.unwrap();
    assert_eq!(accepted.tool_name, "echo");
    assert_eq!(
        accepted.request_doc_id, request_doc_id,
        "accepted binding must not cross physical request identity"
    );

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

    let native = crate::session::load_tool_call_result(
        &crate::store::config_client::ConfigAccess::local(node.clone()),
        &tool_doc_id,
        "did:test:test",
        &session_id,
        None,
    )
    .await
    .expect("delivered tool result");
    assert_eq!(
        crate::tools::call::lifecycle::query::render_tool_result(&native).unwrap(),
        "ECHOED",
    );
}

#[tokio::test]
async fn claude_messages_redacted_and_signed_tool_round_trip_through_owned_loop() {
    let case = crate::lean_vocab_test::lean_prompt_assembly_claude_replay_cases()
        .iter()
        .find(|case| case.name == "streamed-signed-redacted-tool")
        .expect("Lean streamed signed/redacted/tool replay case");
    assert_eq!(case.outcome, "ok", "{}", case.name);
    let expected_content = crate::lean_vocab_test::native_claude_replay_content(&case.blocks);
    let input = crate::inference::clients::claude_messages::build_messages_body_native(
        "claude-sonnet-5",
        None,
        Some(128),
        &[Message::Assistant {
            id: None,
            content: expected_content.clone(),
        }],
        &[],
    )
    .expect("modeled canonical blocks render as Messages input");
    let expected = case
        .replay
        .iter()
        .map(crate::lean_vocab_test::LeanClaudeReplayBlock::native_json)
        .collect::<Vec<_>>();
    let kinds = expected
        .iter()
        .map(|block| block["type"].as_str().expect("block type"))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        ["thinking", "redacted_thinking", "tool_use"],
        "{}: the model case must keep its ordered shape",
        case.name
    );
    let first_turn = claude_sse_for_blocks(
        input["messages"][0]["content"]
            .as_array()
            .expect("modeled Messages content"),
    );

    let scope_kind = gents_protocol::rendered_request::CaptureScopeKind::Inference;
    let ClaudeFixtureRun {
        node,
        lifecycle,
        collected,
        request_commit_cid,
    } = run_claude_fixture_turns(first_turn, scope_kind).await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

    let request = lifecycle.request();
    let headers = crate::store::graphql::graphql_with_transaction_retry(
        &node,
        &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }}, role: {{ _eq: "assistant" }} }}, order: {{ sequence: ASC }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(&request.doc_id),
        ),
        "Claude fixture provider messages",
    )
    .await
    .expect("persisted assistant message headers");
    let headers = headers.data.as_ref().expect("query data")["AgentMessage"]
        .as_array()
        .expect("assistant headers");
    assert_eq!(headers.len(), 2, "both provider turns were persisted");
    let (_, stored) = crate::session::load_canonical_message_from_node(
        &node,
        headers[0]["_docID"]
            .as_str()
            .expect("provider message document"),
        &request.node_did,
        request.requester_did.as_deref(),
    )
    .await
    .expect("canonical first provider turn");
    let Message::Assistant { content, .. } = stored else {
        panic!("provider turn must reconstruct as an assistant message");
    };
    assert_eq!(
        content, expected_content,
        "persisted canonical blocks must retain the modeled stream grouping"
    );

    let sends = captured_claude_sends(
        &node,
        &lifecycle.request().doc_id,
        &request_commit_cid,
        scope_kind,
    )
    .await;
    let mut replayed = sends[1]["messages"]
        .as_array()
        .expect("Messages body")
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("accepted assistant replay in second send")["content"]
        .as_array()
        .expect("assistant blocks")
        .clone();
    for block in &mut replayed {
        block
            .as_object_mut()
            .expect("content object")
            .remove("cache_control");
    }
    assert_eq!(
        replayed, expected,
        "the continuation must replay signed, redacted, and tool blocks in stream order"
    );
}

fn claude_sse_for_blocks(blocks: &[serde_json::Value]) -> String {
    let mut events = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match block["type"].as_str().expect("block type") {
            "thinking" => {
                events.push(
                    serde_json::json!({"type":"content_block_start","index":index,
                    "content_block":{"type":"thinking","thinking":""}}),
                );
                events.push(
                    serde_json::json!({"type":"content_block_delta","index":index,
                    "delta":{"type":"thinking_delta","thinking":block["thinking"]}}),
                );
                events.push(
                    serde_json::json!({"type":"content_block_delta","index":index,
                    "delta":{"type":"signature_delta","signature":block["signature"]}}),
                );
            }
            "redacted_thinking" => {
                events.push(
                    serde_json::json!({"type":"content_block_start","index":index,
                    "content_block":{"type":"redacted_thinking","data":block["data"]}}),
                );
            }
            "tool_use" => {
                events.push(
                    serde_json::json!({"type":"content_block_start","index":index,
                    "content_block":{"type":"tool_use","id":block["id"],"name":block["name"],
                        "input":{}}}),
                );
                events.push(
                    serde_json::json!({"type":"content_block_delta","index":index,
                    "delta":{"type":"input_json_delta",
                        "partial_json":block["input"].to_string()}}),
                );
            }
            other => panic!("no SSE encoding for {other}"),
        }
        events.push(serde_json::json!({"type":"content_block_stop","index":index}));
    }
    events.push(
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
        "usage":{"input_tokens":10,"output_tokens":5}}),
    );
    events.push(serde_json::json!({"type":"message_stop"}));
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

struct ClaudeFixtureRun {
    node: Arc<defra_node::EmbeddedNode>,
    lifecycle: crate::request::lifecycle::RequestLifecycle,
    collected: CollectedScriptedStream,
    request_commit_cid: String,
}

async fn run_claude_fixture_turns(
    first_turn: String,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> ClaudeFixtureRun {
    use crate::inference::clients::claude_messages::fixtures::{
        install_messages_sse_fixtures, lock_fixtures_for_test, sse_fixture_final_text,
    };
    use crate::inference::clients::claude_subscription::{ClaudeSubscriptionClient, StaticBearer};
    use rig::client::CompletionClient;

    let _guard = lock_fixtures_for_test().await;
    install_messages_sse_fixtures(vec![first_turn, sse_fixture_final_text("done")]);
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;

    let model =
        ClaudeSubscriptionClient::with_bearer(Arc::new(StaticBearer::failing("no credential")))
            .completion_model("claude-sonnet-5");
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![echo_tool()]);
    let mut config = owned_config(4);
    config.on_rendered_request = Some(crate::inference::rendered_request::scope::ambient_arming_sink(
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
        crate::inference::llm::backend_client::claude_subscription_replay_issuer()
            .expect("Claude Messages route"),
        crate::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let request_commit_cid = lifecycle
        .request_commit_cid()
        .expect("claimed request commit CID")
        .to_owned();
    let capture_factory =
        crate::inference::rendered_request::defra_rendered_request_capture_factory(node.clone());
    let capture_scope = crate::inference::rendered_request::scope_from_factory(
        crate::inference::rendered_request::context_for_claimed_request(
            lifecycle.request(),
            &request_commit_cid,
            "claude-sonnet-5".to_owned(),
            Some(
                crate::BackendProviderKind::ClaudeCliSubscription
                    .as_str()
                    .to_owned(),
            ),
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
    ClaudeFixtureRun {
        node,
        lifecycle,
        collected,
        request_commit_cid,
    }
}

#[test]
fn anthropic_api_key_client_runs_a_messages_turn_on_fixtures() {
    use crate::inference::clients::claude_messages::fixtures::{
        blocking_lock_fixtures_for_test, install_messages_sse_fixtures, sse_fixture_final_text,
    };
    use crate::inference::llm::backend_client::{build_backend_client, BackendClient};
    use rig::client::CompletionClient;

    let _guard = blocking_lock_fixtures_for_test();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            install_messages_sse_fixtures(vec![sse_fixture_final_text("done")]);
            let (node, hook, writer, mut lifecycle) = owned_test_hook().await;

            let identity = crate::store::identity::KeyIdentity::load_or_create(
                std::env::temp_dir()
                    .join(format!("anthropic-key-turn-{}.key", uuid::Uuid::new_v4())),
                None,
            )
            .unwrap();
            let mut behavior = crate::agent::PendingAgent::new("anthropic-key-turn")
                .build_with_identity_for_test(identity);
            behavior.backend_provider_kind = crate::BackendProviderKind::AnthropicApiKey;
            let client = build_backend_client(std::sync::Arc::default(),
                node.clone(),
                &behavior,
                "placeholder-key",
                std::time::Duration::from_secs(1),
            )
            .await
            .expect("key client builds without I/O");
            assert_eq!(client.provider_family(), "AnthropicApiKey");
            let issuer = client.replay_issuer().expect("Messages route");
            let BackendClient::AnthropicApiKey(client) = client else {
                panic!("expected the AnthropicApiKey client");
            };

            let mut config = owned_config(2);
            config.provider_input_counter =
                Arc::new(crate::provider_input::ProviderInputCounter::new(
                    crate::BackendProviderKind::AnthropicApiKey,
                    crate::OpenAiWireApi::ChatCompletions,
                    "claude-sonnet-5",
                ));
            let scope_kind = gents_protocol::rendered_request::CaptureScopeKind::Inference;
            config.on_rendered_request = Some(crate::inference::rendered_request::scope::ambient_arming_sink(
                scope_kind,
            ));
            let request_commit_cid = lifecycle
                .request_commit_cid()
                .expect("claimed request commit CID")
                .to_owned();
            config.replay = crate::provider_input::replay::owned_replay_input(
                node.clone(),
                lifecycle.request().clone(),
                request_commit_cid.clone(),
                scope_kind,
                issuer,
                crate::provider_input::ProviderInputProfile::ClaudeMessages,
            );
            let capture_factory =
                crate::inference::rendered_request::defra_rendered_request_capture_factory(node.clone());
            let capture_scope = crate::inference::rendered_request::scope_from_factory(
                crate::inference::rendered_request::context_for_claimed_request(
                    lifecycle.request(),
                    &request_commit_cid,
                    "claude-sonnet-5".to_owned(),
                    Some("AnthropicApiKey".to_owned()),
                ),
                Some(&capture_factory),
            )
            .expect("DefraDB rendered-request capture scope");
            let stream = run_loop_stream(
                client.completion_model("claude-sonnet-5"),
                Some(hook.clone()),
                TaggedMessage::unassociated(Message::user("say done")),
                Vec::new(),
                Arc::new(Vec::new()),
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
            assert_eq!(collected.error, None);
            assert_eq!(collected.final_text.as_deref(), Some("done"));
        });
}

async fn captured_claude_sends(
    node: &Arc<defra_node::EmbeddedNode>,
    request_doc_id: &str,
    request_commit_cid: &str,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> Vec<serde_json::Value> {
    let captures = node
        .execute(&format!(
            r#"{{ RenderedRequest(filter: {{ request_doc_id: {{ _eq: "{}" }} }}, order: {{ turn_index: ASC }}) {{ turn_index capture_scope source request_commit_cid capture_version request_json }} }}"#,
            crate::store::graphql::escape_graphql_string(request_doc_id),
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
    let mut bodies = Vec::with_capacity(capture_rows.len());
    for (turn_index, row) in capture_rows.iter().enumerate() {
        assert_eq!(row["capture_scope"], format!("{scope_kind}.1"));
        assert_eq!(
            row["turn_index"].as_u64(),
            Some(turn_index as u64),
            "capture turn coordinate"
        );
        assert_eq!(row["source"], expected_source, "Claude origin is physical");
        assert_eq!(row["request_commit_cid"].as_str(), Some(request_commit_cid));
        let (body, _) = crate::inference::rendered_request::decode_capture_pair(
            &crate::store::config_client::ConfigAccess::local(node.clone()),
            u32::try_from(row["capture_version"].as_u64().expect("capture version"))
                .expect("version fits u32"),
            row["request_json"].as_str().expect("encoded request JSON"),
        )
        .await
        .expect("decode the physical Claude capture");
        bodies.push(body);
    }
    bodies
}

#[tokio::test]
async fn claude_refusal_after_partial_text_ends_after_one_attempt() {
    // Recorded shape of a streaming refusal that follows partial output: a
    // text block opens (a provider audit event) and streams a delta, then the
    // turn closes with stop_reason "refusal". The stream has already produced
    // items, yet the refusal is deterministic, so the loop must fail on this
    // attempt with the named refusal instead of retracting and resampling.
    let refusal = [
        serde_json::json!({"type":"message_start","message":{"id":"msg_refusal",
            "role":"assistant","content":[],"stop_reason":null,
            "usage":{"input_tokens":4,"output_tokens":1}}}),
        serde_json::json!({"type":"content_block_start","index":0,
            "content_block":{"type":"text","text":""}}),
        serde_json::json!({"type":"content_block_delta","index":0,
            "delta":{"type":"text_delta","text":"I can help with part of"}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"message_delta",
            "delta":{"stop_reason":"refusal","stop_sequence":null},
            "usage":{"output_tokens":6}}),
        serde_json::json!({"type":"message_stop"}),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();

    let run = run_claude_fixture_turns(
        refusal,
        gents_protocol::rendered_request::CaptureScopeKind::Inference,
    )
    .await;
    let collected = run.collected;

    assert_eq!(
        collected.text_chunks,
        vec!["I can help with part of".to_string()],
        "the partial text streamed before the refusal"
    );
    assert!(
        collected.retractions.is_empty(),
        "a refusal is not retracted and resampled: {:?}",
        collected.retractions
    );
    assert_eq!(collected.attempts.len(), 1, "{:?}", collected.attempts);
    assert!(
        !collected.attempts[0].will_retry,
        "the refusal attempt is terminal"
    );
    let error = collected.error.expect("the refusal ends the request");
    assert!(
        error.contains("stop_reason: refusal"),
        "the terminal error names the refusal: {error}"
    );
    assert!(
        !error.contains("no visible output") && !error.contains("retry budget exhausted"),
        "the refusal is not reported as a transport failure: {error}"
    );
}
