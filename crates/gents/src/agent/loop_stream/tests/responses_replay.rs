/// Two Responses turns through the owned loop against a local SSE server:
/// an encrypted reasoning item plus `function_call echo` → gents executes
/// echo → the continuation replays the reasoning item's exact
/// `encrypted_content`, resolved from the first turn's real DefraDB capture →
/// text `done`.
#[tokio::test]
async fn responses_encrypted_reasoning_round_trip_through_owned_loop() {
    use axum::extract::State;
    use axum::routing::post;
    use rig::client::CompletionClient;
    use std::sync::Mutex as StdMutex;

    fn sse(events: &[serde_json::Value]) -> String {
        events
            .iter()
            .map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap()))
            .collect()
    }
    fn completed(output: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"type":"response.completed","sequence_number":9,"response":{
            "id":"resp_1","object":"response","created_at":1,"status":"completed",
            "error":null,"incomplete_details":null,"instructions":null,
            "max_output_tokens":null,"model":"gpt-test","output":output,
            "usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":0},
                "output_tokens":5,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":15}}})
    }
    let reasoning = serde_json::json!({"type":"reasoning","id":"rs_1",
        "summary":[{"type":"summary_text","text":"plan"}],"encrypted_content":"sealed-1"});
    let call = serde_json::json!({"type":"function_call","id":"fc_1","call_id":"call_1",
        "name":"echo","arguments":"{\"text\":\"hi\"}","status":"completed"});
    let message = serde_json::json!({"type":"message","id":"msg_2","role":"assistant",
        "status":"completed","content":[{"type":"output_text","text":"done","annotations":[]}]});
    let turns = vec![
        sse(&[
            serde_json::json!({"type":"response.output_item.done","output_index":0,"sequence_number":1,"item":reasoning}),
            serde_json::json!({"type":"response.output_item.added","output_index":1,"sequence_number":2,"item":call}),
            serde_json::json!({"type":"response.output_item.done","output_index":1,"sequence_number":3,"item":call}),
            completed(serde_json::json!([reasoning, call])),
        ]),
        sse(&[
            serde_json::json!({"type":"response.output_text.delta","item_id":"msg_2","output_index":0,"content_index":0,"sequence_number":1,"delta":"done"}),
            serde_json::json!({"type":"response.output_item.done","output_index":0,"sequence_number":2,"item":message}),
            completed(serde_json::json!([message])),
        ]),
    ];
    type Server = Arc<StdMutex<(std::collections::VecDeque<String>, Vec<serde_json::Value>)>>;
    let state: Server = Arc::new(StdMutex::new((turns.into(), Vec::new())));
    async fn respond(
        State(state): State<Server>,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        let mut state = state.lock().unwrap();
        state.1.push(serde_json::from_slice(&body).unwrap_or_default());
        let body = state.0.pop_front().unwrap_or_default();
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(body))
            .unwrap()
    }
    let app = axum::Router::new()
        .route("/v1/responses", post(respond))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = crate::inference_http::build_openai_responses_client(
        "test-key",
        &format!("http://{address}/v1"),
        crate::inference_http::SessionTaggingHttpClient::new(
            crate::inference_http::ResponsesNormalizingHttpClient::new(
                crate::rendered_request::RenderedRequestCapturingHttpClient::<
                    crate::provider_http::ProviderHttpClient,
                >::default(),
            ),
        ),
        Default::default(),
    )
    .unwrap();
    let backend = crate::llm::backend_client::BackendClient::OpenAiResponses(client);
    let issuer = backend.replay_issuer().unwrap().expect("Responses route");
    let family = backend.provider_family().to_owned();
    let crate::llm::backend_client::BackendClient::OpenAiResponses(client) = backend else {
        unreachable!("constructed above")
    };
    let model = client.completion_model("gpt-test");

    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let scope_kind = gents_protocol::rendered_request::CaptureScopeKind::Inference;
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![echo_tool()]);
    let mut config = owned_config(4);
    config.on_rendered_request = Some(crate::rendered_request::scope::ambient_arming_sink(
        scope_kind,
    ));
    config.provider_input_counter = Arc::new(crate::provider_input::ProviderInputCounter::new(
        crate::BackendProviderKind::OpenAiCompatible,
        crate::OpenAiWireApi::Responses,
        "gpt-test",
    ));
    let profile = config.provider_input_counter.profile();
    let request_commit_cid = lifecycle
        .request_commit_cid()
        .expect("claimed request commit CID")
        .to_owned();
    config.replay = crate::provider_input::replay::owned_replay_input(
        node.clone(),
        lifecycle.request().clone(),
        request_commit_cid.clone(),
        scope_kind,
        Some(issuer),
        profile,
    );
    let capture_factory =
        crate::rendered_request::defra_rendered_request_capture_factory(node.clone());
    let capture_scope = crate::rendered_request::scope_from_factory(
        crate::rendered_request::context_for_claimed_request(
            lifecycle.request(),
            &request_commit_cid,
            "gpt-test".to_owned(),
            Some(family),
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
        profile,
        Some(capture_scope),
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

    let bodies = state.lock().unwrap().1.clone();
    assert_eq!(bodies.len(), 2, "two provider sends");
    let replayed = bodies[1]["input"]
        .as_array()
        .expect("Responses input")
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .map(|item| item["encrypted_content"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        replayed,
        vec![serde_json::json!("sealed-1")],
        "the continuation replays the exact encrypted reasoning: {}",
        bodies[1]
    );
}
