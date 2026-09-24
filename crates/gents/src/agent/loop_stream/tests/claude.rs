/// Two Messages turns through the owned loop on SSE fixtures: `tool_use echo`
/// with streamed arguments → gents executes echo → `tool_result` continuation
/// → text `done`. The streamed native arguments are asserted through the
/// canonical admission binding (`load_accepted_for_dispatch`) and exact
/// reconstruction of its accepted `ToolArguments` payload — canonical
/// admission resolves a terminal (completed) call too. The exact delivered
/// output is asserted through `session::load_tool_call_result` (defect C1,
/// live-confirmed by write request #8).
#[tokio::test]
async fn claude_messages_tool_round_trip_through_owned_loop() {
    use crate::claude_messages::{
        install_messages_sse_fixtures, lock_fixtures_for_test, sse_fixture_final_text,
        sse_fixture_tool_use,
    };
    use crate::claude_subscription::{ClaudeSubscriptionClient, StaticBearer};
    use rig::client::CompletionClient;

    let _guard = lock_fixtures_for_test();
    install_messages_sse_fixtures(vec![
        sse_fixture_tool_use("toolu_1", "echo", "{\"text\":\"hi\"}"),
        sse_fixture_final_text("done"),
    ]);
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;

    // Fixture-only: a refusing bearer keeps an extra turn off the network.
    let model =
        ClaudeSubscriptionClient::with_bearer(Arc::new(StaticBearer::failing("no credential")))
            .completion_model("claude-sonnet-5");
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![echo_tool()]);

    let stream = run_loop_stream(
        model,
        Some(hook.clone()),
        Message::user("use the echo tool"),
        Vec::new(),
        tools,
        owned_config(4),
    );
    let collected = collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle).await;
    assert!(collected.error.is_none(), "{:?}", collected.error);

    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

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
    let request_doc_id = lifecycle.request().doc_id.clone();

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
