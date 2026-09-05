// Included in inference.rs's test module to reuse its real daemon harness.
#[derive(Clone)]
struct NonTerminalProvider {
    empty_forever: bool,
    stream_calls: Arc<AtomicUsize>,
    empty_deltas: Arc<AtomicUsize>,
}

#[allow(refining_impl_trait)]
impl CompletionModel for NonTerminalProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        Self {
            empty_forever: false,
            stream_calls: Arc::new(AtomicUsize::new(0)),
            empty_deltas: Arc::new(AtomicUsize::new(0)),
        }
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming regression only".into(),
        ))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        // The fake provider still crosses the production capture transport:
        // serialize its actual input, persist it, then emit its scripted output.
        use rig::http_client::HttpClientExt;
        let dto = rig::providers::openai::completion::CompletionRequest::try_from((
            "scripted".to_owned(),
            request,
        ))
        .map_err(|error| CompletionError::ProviderError(error.to_string()))?;
        let mut body = serde_json::to_value(dto)
            .map_err(|error| CompletionError::ProviderError(error.to_string()))?;
        body["stream"] = serde_json::Value::Bool(true);
        body["stream_options"] = serde_json::json!({ "include_usage": true });
        let inner = crate::rendered_request::transport::CountingInner::default();
        let transport = crate::rendered_request::transport::RenderedRequestCapturingHttpClient::new(
            inner.clone(),
        );
        let outbound = rig::http_client::Request::builder()
            .method("POST")
            .uri("https://scripted-provider.invalid/v1/chat/completions")
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(serde_json::to_vec(&body).map_err(
                |error| CompletionError::ProviderError(error.to_string()),
            )?))
            .map_err(|error| CompletionError::ProviderError(error.to_string()))?;
        let _response = transport
            .send_streaming(outbound)
            .await
            .map_err(|error| CompletionError::ProviderError(error.to_string()))?;
        assert_eq!(
            inner.send_count(),
            1,
            "capture must authorize the scripted provider send"
        );

        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let endless = self.empty_forever;
        let empty_deltas = self.empty_deltas.clone();
        let inner: rig::streaming::StreamingResult<()> =
            Box::pin(stream::unfold(0usize, move |index| {
                let empty_deltas = empty_deltas.clone();
                async move {
                    if index == 0 {
                        Some((
                            Ok(RawStreamingChoice::Message(
                                "durable partial incident text".into(),
                            )),
                            1,
                        ))
                    } else if endless {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        empty_deltas.fetch_add(1, Ordering::SeqCst);
                        Some((Ok(RawStreamingChoice::Message(String::new())), index + 1))
                    } else {
                        None
                    }
                }
            }));
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

async fn eight_nonterminal_requests_converge_on_same_daemon(empty_forever: bool) {
    let data = tempfile::tempdir().unwrap();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(data.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let mut behavior = test_behavior();
    Arc::get_mut(&mut behavior).unwrap().stream_liveness_timeout = Duration::from_secs(1);
    let agent_did = behavior.agent_did().to_owned();
    let identity = behavior.principal_identity().clone();
    let model = NonTerminalProvider {
        empty_forever,
        stream_calls: Arc::new(AtomicUsize::new(0)),
        empty_deltas: Arc::new(AtomicUsize::new(0)),
    };
    let prompt = LayeredPromptBuilder::for_behavior(
        &behavior.system_prompt,
        &behavior.behavior_id,
        &[],
        false,
        &[],
    );
    let mut daemon = BehaviorDaemon::new(
        node.clone(),
        behavior.clone(),
        Arc::new(model.clone()),
        prompt.preamble().to_owned(),
        Arc::new(Vec::new()),
        prompt,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), agent_did.clone()),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    );
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    for _ in 0..8 {
        let request = create_routed_request(&node, &behavior, &agent_did).await;
        // This test counts request inference attempts exactly. A supplied title
        // prevents optional background title generation from sharing the same
        // deliberately nonterminal provider and inflating calls/captures.
        let session_id = crate::graphql::escape_graphql_string(&request.session_id);
        let behavior_id = crate::graphql::escape_graphql_string(&behavior.behavior_id);
        let escaped_agent_did = crate::graphql::escape_graphql_string(&agent_did);
        let requester =
            crate::session::requester_did_create_field(request.requester_did.as_deref());
        let seeded = node.execute(&format!(
            r#"mutation {{ create_AgentConversation(input: {{
                session_id: "{session_id}", agent_name: "{behavior_id}", agent_did: "{escaped_agent_did}",
                behavior_id: "{behavior_id}", {requester} title: "lease regression", title_source: "task",
                preview_text: "route this reply", status: "active",
                created_at: "{}", updated_at: "{}"
            }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&request.created_at),
            crate::graphql::escape_graphql_string(&request.created_at),
        )).await;
        assert!(!seeded.has_errors(), "{:?}", seeded.errors);
        let request_id = crate::graphql::escape_graphql_string(&request.request_id);
        let query = format!(
            r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ lifecycle_state execution_progress_seq }}
            AgentResponse(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ status content }}
            AgentMessage(filter: {{ request_id: {{ _eq: "{request_id}" }}, role: {{ _eq: "assistant" }} }}) {{ content }}
        }}"#
        );
        tokio::time::timeout(Duration::from_secs(15), async {
            // Drive the same maintenance owner used by a running runtime. No
            // daemon or database restart occurs between these eight requests.
            let process = daemon.process_request(request, shutdown_rx.clone());
            tokio::pin!(process);
            // Maintenance must stay independently pollable: `process` can hold
            // the node write gate while a recovery pass waits on it, so
            // awaiting `recover_all` inline in a select branch self-deadlocks.
            // The scoped binding drops the pinned maintenance future the
            // moment process completes, cancelling any in-flight recovery.
            {
                let maintenance = async {
                    loop {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        crate::RequestLifecycle::recover_all(&node, &agent_did)
                            .await
                            .unwrap();
                    }
                };
                tokio::pin!(maintenance);
                tokio::select! {
                    _ = &mut process => {}
                    _ = &mut maintenance => {}
                }
            }
            loop {
                crate::RequestLifecycle::recover_all(&node, &agent_did)
                    .await
                    .unwrap();
                let result = node.execute(&query).await;
                assert!(!result.has_errors(), "{:?}", result.errors);
                let data = result.data.as_ref().unwrap();
                let request = &data["AgentRequest"][0];
                if matches!(
                    request["lifecycle_state"].as_str(),
                    Some("claimed" | "processing")
                ) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                assert_eq!(
                    request["lifecycle_state"], "failed",
                    "nonterminal provider stream must never complete: {data}"
                );
                assert_eq!(
                    data["AgentResponse"].as_array().unwrap().len(),
                    1,
                    "one response per execution"
                );
                assert_eq!(data["AgentResponse"][0]["status"], "error");
                assert!(
                    data["AgentResponse"][0]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("durable partial incident text")
                        || data["AgentMessage"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|row| row["content"]
                                .as_str()
                                .unwrap_or_default()
                                .contains("durable partial incident text")),
                    "terminalization must preserve durable partial output: {data}"
                );
                // Empty deltas cannot keep incrementing semantic progress.
                assert!(
                    request["execution_progress_seq"].as_i64().unwrap() < 10,
                    "empty deltas renewed the execution lease: {data}"
                );
                let second = crate::RequestLifecycle::recover_all(&node, &agent_did)
                    .await
                    .unwrap();
                assert_eq!(second.requests_recovered, 0);
                assert_eq!(second.responses_recovered, 0);
                break;
            }
        })
        .await
        .expect("active request must converge without restarting the daemon");
    }
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 8);
    let captures = node.execute("{ RenderedRequest { _docID } }").await;
    assert!(!captures.has_errors(), "{:?}", captures.errors);
    assert_eq!(
        captures.data.as_ref().unwrap()["RenderedRequest"]
            .as_array()
            .unwrap()
            .len(),
        8,
        "every fake provider call must have crossed the durable capture transport"
    );
    if empty_forever {
        assert!(
            model.empty_deltas.load(Ordering::SeqCst) >= 8,
            "provider emitted traffic after its last semantic progress"
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn eight_partial_provider_eofs_fail_and_preserve_progress_without_restart() {
    eight_nonterminal_requests_converge_on_same_daemon(false).await;
}

#[tokio::test]
async fn eight_infinite_empty_provider_streams_expire_semantic_leases_without_restart() {
    eight_nonterminal_requests_converge_on_same_daemon(true).await;
}
