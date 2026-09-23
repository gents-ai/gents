// Included in inference.rs's test module to reuse its real daemon harness.
#[derive(Clone)]
struct NonTerminalProvider {
    empty_forever: bool,
    active_chunks: Option<usize>,
    stream_calls: Arc<AtomicUsize>,
    empty_deltas: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct LeaseLossProvider;

#[allow(refining_impl_trait)]
impl CompletionModel for LeaseLossProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();
    fn make(_: &(), _: impl Into<String>) -> Self {
        Self
    }
    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("stream only".into()))
    }
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        crate::test_support::capture_scripted_provider_request(&request, "lease-loss").await?;
        let events = vec![
            Ok(RawStreamingChoice::ToolCall(
                rig::streaming::RawStreamingToolCall::new(
                    "lease-tool".into(),
                    "lease_block".into(),
                    serde_json::json!({}),
                ),
            )),
            Ok(RawStreamingChoice::FinalResponse(())),
        ];
        Ok(StreamingCompletionResponse::stream(Box::pin(stream::iter(
            events,
        ))))
    }
}

#[derive(Clone)]
struct LeaseBlockingTool;

impl crate::llm::tool::ToolDyn for LeaseBlockingTool {
    fn name(&self) -> String {
        "lease_block".into()
    }
    fn definition<'a>(
        &'a self,
        _: String,
    ) -> crate::llm::tool::BoxFuture<'a, crate::llm::tool::ToolDefinition> {
        Box::pin(async {
            crate::llm::tool::ToolDefinition {
                name: "lease_block".into(),
                description: "wait for lease replacement".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(
        &'a self,
        _: String,
    ) -> crate::llm::tool::BoxFuture<'a, Result<String, crate::llm::tool::ToolError>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn lease_poll_ownership_loss_does_not_fail_a_running_tool() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let behavior = test_behavior();
    let agent_did = behavior.agent_did().to_owned();
    let identity = behavior.principal_identity().clone();
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
        Arc::new(LeaseLossProvider),
        prompt.preamble().to_owned(),
        Arc::new(vec![
            Box::new(LeaseBlockingTool) as Box<dyn crate::llm::tool::ToolDyn>
        ]),
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
    )
    .unwrap();
    let request = create_routed_request(&node, &behavior, &agent_did).await;
    let request_doc_id = request.doc_id.clone();
    let session = gents_protocol::session::AgentSession {
        session_id: request.session_id.clone(),
        agent_did: agent_did.clone(),
        requester_did: request.requester_did.clone(),
        behavior_id: behavior.behavior_id.clone(),
        created_at: request.created_at.clone(),
        closed_at: None,
        title: Some(gents_protocol::session::SessionTitle {
            text: "lease loss tool regression".into(),
            source: gents_protocol::session::SessionTitleSource::Task,
        }),
        tags: vec![],
        provenance: None,
        observation: None,
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session).unwrap())
            .unwrap();
    let seeded = node
        .execute(&format!(
            "mutation {{ create_AgentSession(input: {input}) {{_docID}} }}"
        ))
        .await;
    assert!(!seeded.has_errors(), "{:?}", seeded.errors);
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let process = daemon.process_request(request, shutdown_rx);
    tokio::pin!(process);
    tokio::time::timeout(Duration::from_secs(10), async {
        let observe_running = async {
            loop {
                let response = node.execute("{ AgentToolCall(filter:{tool_name:{_eq:\"lease_block\"}}){_docID lifecycle_state tool_failure_class cancel_cause} }").await;
                let rows = response.data.as_ref().unwrap()["AgentToolCall"].as_array().unwrap();
                if rows.first().is_some_and(|row| row["lifecycle_state"] == "running") { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::pin!(observe_running);
        tokio::select! {
            _ = &mut observe_running => {}
            _ = &mut process => panic!("daemon exited before the accepted tool reached running"),
        }
        let escaped = crate::graphql::escape_graphql_string(&request_doc_id);
        let replaced = node.execute(&format!(r#"mutation {{ update_AgentRequest(filter:{{_docID:{{_eq:"{escaped}"}}}},input:{{execution_generation:"replacement-generation"}}){{_docID}} }}"#)).await;
        assert!(!replaced.has_errors(), "{:?}", replaced.errors);
        (&mut process).await;
    }).await.expect("lease poll must observe replacement");
    let response = node.execute("{ AgentToolCall(filter:{tool_name:{_eq:\"lease_block\"}}){lifecycle_state tool_failure_class cancel_cause} }").await;
    let row = &response.data.as_ref().unwrap()["AgentToolCall"][0];
    assert_eq!(row["lifecycle_state"], "running");
    assert!(row["tool_failure_class"].is_null());
    assert!(row["cancel_cause"].is_null());
}

#[allow(refining_impl_trait)]
impl CompletionModel for NonTerminalProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        Self {
            empty_forever: false,
            active_chunks: None,
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
        crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;

        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let endless = self.empty_forever;
        let active_chunks = self.active_chunks;
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
                    } else if index <= active_chunks.unwrap_or(0) {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        Some((
                            Ok(RawStreamingChoice::Message(format!(" chunk-{index}"))),
                            index + 1,
                        ))
                    } else if active_chunks.is_some_and(|chunks| index == chunks + 1) {
                        Some((Ok(RawStreamingChoice::FinalResponse(())), index + 1))
                    } else if active_chunks.is_some() {
                        None
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
        active_chunks: None,
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
    )
    .expect("construct daemon with valid execution configuration");
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    for _ in 0..8 {
        let request = create_routed_request(&node, &behavior, &agent_did).await;
        // This test counts request inference attempts exactly. A supplied title
        // prevents optional background title generation from sharing the same
        // deliberately nonterminal provider and inflating calls/captures.
        let session = gents_protocol::session::AgentSession {
            session_id: request.session_id.clone(),
            agent_did: agent_did.clone(),
            requester_did: request.requester_did.clone(),
            behavior_id: behavior.behavior_id.clone(),
            created_at: request.created_at.clone(),
            closed_at: None,
            title: Some(gents_protocol::session::SessionTitle {
                text: "lease regression".into(),
                source: gents_protocol::session::SessionTitleSource::Task,
            }),
            tags: vec![],
            provenance: None,
            observation: None,
        };
        let input = gents_protocol::graphql::graphql_input_literal(
            &serde_json::to_value(session).expect("canonical session fixture"),
        )
        .expect("render canonical session input");
        let seeded = node
            .execute(&format!(
                "mutation {{ create_AgentSession(input: {input}) {{_docID}} }}"
            ))
            .await;
        assert!(!seeded.has_errors(), "{:?}", seeded.errors);
        let request_doc_id = request.doc_id.clone();
        let session_id = request.session_id.clone();
        let requester_did = request.requester_did.clone();
        let escaped_doc = crate::graphql::escape_graphql_string(&request_doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc}" }} }}) {{
                lifecycle_state execution_generation execution_lease_expires_at terminal_output
            }} }}"#
        );
        tokio::time::timeout(Duration::from_secs(15), async {
            let process = daemon.process_request(request, shutdown_rx.clone());
            tokio::pin!(process);
            let mut observed_renewal = false;
            {
                // Recovery and observation remain independently pollable while
                // the owning process waits for provider input.
                let maintenance = async {
                    let mut initial_owner: Option<(String, chrono::DateTime<chrono::FixedOffset>)> =
                        None;
                    loop {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let recovered = crate::RequestLifecycle::recover_all(&node, &agent_did)
                            .await
                            .unwrap();
                        if !empty_forever || observed_renewal {
                            continue;
                        }
                        let result = node.execute(&query).await;
                        assert!(!result.has_errors(), "{:?}", result.errors);
                        let row = &result.data.as_ref().unwrap()["AgentRequest"][0];
                        if row["lifecycle_state"] != "processing" {
                            continue;
                        }
                        let generation = row["execution_generation"]
                            .as_str()
                            .expect("claimed generation");
                        let expiry = chrono::DateTime::parse_from_rfc3339(
                            row["execution_lease_expires_at"]
                                .as_str()
                                .expect("stored lease deadline"),
                        )
                        .unwrap();
                        let (initial_generation, initial_expiry) =
                            initial_owner.get_or_insert_with(|| (generation.to_owned(), expiry));
                        if chrono::Utc::now() <= *initial_expiry {
                            continue;
                        }
                        assert_eq!(
                            generation, initial_generation.as_str(),
                            "renewal must preserve ownership"
                        );
                        assert!(
                            expiry > *initial_expiry && expiry > chrono::Utc::now(),
                            "silent owner must renew beyond its original stored deadline"
                        );
                        assert_eq!(
                            recovered.requests_recovered, 0,
                            "recovery must not steal a silently renewing owner"
                        );
                        observed_renewal = true;
                        crate::interrupt::interrupt_request_by_doc_id(
                            &node,
                            &request_doc_id,
                            &agent_did,
                            requester_did.as_deref(),
                        )
                        .await
                        .unwrap();
                    }
                };
                tokio::pin!(maintenance);
                tokio::select! {
                    _ = &mut process => {}
                    _ = &mut maintenance => unreachable!("maintenance is continuous"),
                }
            }
            assert_eq!(observed_renewal, empty_forever);
            let result = node.execute(&query).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
            let row = &result.data.as_ref().unwrap()["AgentRequest"][0];
            assert_eq!(
                row["lifecycle_state"],
                if empty_forever {
                    "interrupted"
                } else {
                    "failed"
                },
                "silent work needs explicit interruption; premature EOF fails"
            );
            let selected: gents_protocol::output::TerminalOutput =
                serde_json::from_value(row["terminal_output"].clone()).expect("terminal selection");
            match selected {
                gents_protocol::output::TerminalOutput::Message { message_doc_id } => {
                    let (header, native) = crate::session::load_canonical_message_from_node(
                        &node, &message_doc_id, &agent_did, requester_did.as_deref(),
                    ).await.unwrap();
                    assert_eq!(header.request_doc_id.as_deref(), Some(request_doc_id.as_str()));
                    assert_eq!(header.outcome, gents_protocol::output::OutputOutcome::Partial);
                    assert!(gents_protocol::transcript::present_message(&native).body_markdown
                        .contains("durable partial incident text"));
                }
                gents_protocol::output::TerminalOutput::NoMessage => {
                    // Closed unaccepted bytes may remain diagnostics without
                    // becoming a published answer. Verify that distinction
                    // through the shared projection, not a raw payload search.
                    use gents_protocol::output::live::{project_live, LiveObservation, LiveTarget, LiveView, OwnerLiveness};
                    use gents_protocol::output::reconstruction::ObservedSegment;
                    use crate::session::canonical_rows::{AGENT_OUTPUT_SEGMENT_FIELDS, decode_output_segment_row};
                    let result = node.execute(&format!(r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{escaped_doc}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#)).await;
                    assert!(!result.has_errors(), "{:?}", result.errors);
                    let records = result.data.as_ref().unwrap()["AgentOutputSegment"].as_array().unwrap()
                        .iter().map(|row| decode_output_segment_row(row).unwrap()).collect::<Vec<_>>();
                    let provider = records.iter().filter(|row| matches!(row.segment.source,
                        gents_protocol::output::OutputSource::ProviderTurn { .. })).collect::<Vec<_>>();
                    let target = provider.first().expect("retained provider source");
                    assert!(provider.iter().all(|row| row.segment.source == target.segment.source
                        && row.segment.writer == target.segment.writer));
                    let facts = records.iter().map(|row| ObservedSegment {
                        doc_id: &row.doc_id, segment: &row.segment,
                    }).collect::<Vec<_>>();
                    let view = project_live(&LiveObservation {
                        request_doc_id: &request_doc_id, session_id: &session_id,
                        target: LiveTarget { request_doc_id: &request_doc_id,
                            source: &target.segment.source, writer: &target.segment.writer, message_id: None },
                        messages: &[], agent_did: &agent_did, requester_did: requester_did.as_deref(),
                        records: &facts, denied_headers: &[], denied_segments: &[], dependency_denials: &[],
                        owner: OwnerLiveness::default(), request_terminal: true,
                        terminal_selection: Some(gents_protocol::output::TerminalOutput::NoMessage),
                    });
                    let LiveView::RetainedPartial { streams } = view else {
                        panic!("closed bytes must remain retained diagnostics, not live or missing: {view:?}");
                    };
                    assert!(streams.iter().any(|stream| stream.text.contains("durable partial incident text")));
                }
            }
            let second = crate::RequestLifecycle::recover_all(&node, &agent_did)
                .await
                .unwrap();
            assert_eq!(second.requests_recovered, 0);
        })
        .await
        .expect("owned request must converge without restarting the daemon");
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
async fn eight_silent_provider_streams_renew_until_explicitly_interrupted_without_restart() {
    eight_nonterminal_requests_converge_on_same_daemon(true).await;
}

#[tokio::test]
async fn nonempty_stream_outlives_multiple_short_leases_with_default_batching() {
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
    {
        let behavior = Arc::get_mut(&mut behavior).unwrap();
        behavior.stream_batch_ms = crate::config::DEFAULT_STREAM_BATCH_MS;
        behavior.stream_liveness_timeout = Duration::from_secs(1);
    }
    let agent_did = behavior.agent_did().to_owned();
    let identity = behavior.principal_identity().clone();
    let model = NonTerminalProvider {
        empty_forever: false,
        active_chunks: Some(8),
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
    )
    .unwrap();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let request = create_routed_request(&node, &behavior, &agent_did).await;
    let request_id = request.request_id.clone();
    let session = gents_protocol::session::AgentSession {
        session_id: request.session_id.clone(),
        agent_did: agent_did.clone(),
        requester_did: request.requester_did.clone(),
        behavior_id: behavior.behavior_id.clone(),
        created_at: request.created_at.clone(),
        closed_at: None,
        title: Some(gents_protocol::session::SessionTitle {
            text: "active short lease".into(),
            source: gents_protocol::session::SessionTitleSource::Task,
        }),
        tags: vec![],
        provenance: None,
        observation: None,
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session).unwrap())
            .unwrap();
    let seeded = node
        .execute(&format!(
            "mutation {{ create_AgentSession(input: {input}) {{_docID}} }}"
        ))
        .await;
    assert!(!seeded.has_errors(), "{:?}", seeded.errors);

    tokio::time::timeout(
        Duration::from_secs(10),
        daemon.process_request(request, shutdown_rx),
    )
    .await
    .expect("active stream completes across multiple lease durations");

    let request_id = crate::graphql::escape_graphql_string(&request_id);
    let result = node
        .execute(&format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ lifecycle_state execution_progress_seq }}
                AgentMessage(filter: {{ request_id: {{ _eq: "{request_id}" }}, role: {{ _eq: "assistant" }} }}) {{ content }}
            }}"#
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let data = result.data.unwrap();
    assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "completed");
    assert!(
        data["AgentRequest"][0]["execution_progress_seq"]
            .as_i64()
            .unwrap()
            >= 4,
        "durable nonempty snapshots must renew the lease: {data}"
    );
    let content = data["AgentMessage"][0]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.contains("chunk-8"),
        "final output was truncated: {data}"
    );
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);
    node.shutdown().await;
}
