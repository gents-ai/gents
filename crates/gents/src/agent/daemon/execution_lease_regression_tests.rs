// Included in inference.rs's test module to reuse its real daemon harness.
use std::sync::atomic::AtomicBool;

#[derive(Clone)]
struct NonTerminalProvider {
    empty_forever: bool,
    active_chunks: Option<usize>,
    reasoning_prefix: bool,
    stream_calls: Arc<AtomicUsize>,
    empty_deltas: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct LeaseLossProvider;

#[derive(Clone)]
struct BufferedReasoningShutdownProvider {
    processed_two: Arc<tokio::sync::Notify>,
}

#[allow(refining_impl_trait)]
impl CompletionModel for BufferedReasoningShutdownProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        Self {
            processed_two: Arc::new(tokio::sync::Notify::new()),
        }
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
        crate::test_support::capture_scripted_provider_request(&request, "shutdown-reasoning")
            .await?;
        let processed_two = self.processed_two.clone();
        let events: rig::streaming::StreamingResult<()> =
            Box::pin(stream::unfold(0usize, move |index| {
                let processed_two = processed_two.clone();
                async move {
                    match index {
                        0 | 1 => Some((
                            Ok(RawStreamingChoice::ReasoningDelta {
                                id: None,
                                reasoning: if index == 0 { "first" } else { " second" }.into(),
                            }),
                            index + 1,
                        )),
                        _ => {
                            processed_two.notify_one();
                            std::future::pending().await
                        }
                    }
                }
            }));
        Ok(StreamingCompletionResponse::stream(events))
    }
}

async fn graceful_shutdown_reasoning_case(fail_drain: bool, prior_ownership_loss: Option<&str>) {
    use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
    use gents_protocol::output::{OutputOutcome, PayloadRef, SourceClose, StreamPayload};

    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let mut agent_config = test_agent();
    {
        let agent_config = Arc::get_mut(&mut agent_config).unwrap();
        agent_config.stream_batch_ms = 300_000;
        agent_config.deadline_duration = Duration::from_secs(120);
        agent_config.stream_liveness_timeout = Duration::from_secs(120);
        agent_config.provider_idle_timeout = Duration::from_secs(120);
    }
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let model = BufferedReasoningShutdownProvider {
        processed_two: Arc::new(tokio::sync::Notify::new()),
    };
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let mut daemon = AgentDaemon::new(
        node.clone(),
        agent_config.clone(),
        None,
        Arc::new(model.clone()),
        prompt.preamble().to_owned(),
        Arc::new(Vec::new()),
        prompt,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), node_did.clone()),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap();
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let doc_id = request.doc_id.clone();
    let session = gents_protocol::session::AgentSession {
        session_id: request.session_id.clone(),
        node_did: node_did.clone(),
        requester_did: request.requester_did.clone(),
        agent_id: agent_config.agent_id.clone(),
        created_at: request.created_at.clone(),
        closed_at: None,
        title: Some(gents_protocol::session::SessionTitle {
            text: "shutdown reasoning regression".into(),
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

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let process = daemon.process_request(request, shutdown_rx);
    tokio::pin!(process);
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = model.processed_two.notified() => {},
            _ = &mut process => panic!("request exited before both reasoning chunks were processed"),
        }
    }).await.expect("both provider chunks must be processed");
    let escaped = crate::graphql::escape_graphql_string(&doc_id);
    let query = format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
    );
    let before = node.execute(&query).await;
    assert!(!before.has_errors(), "{:?}", before.errors);
    assert!(
        before.data.as_ref().unwrap().to_string().contains("first"),
        "first chunk must be durable before shutdown"
    );
    assert!(
        !before
            .data
            .as_ref()
            .unwrap()
            .to_string()
            .contains(" second"),
        "second received chunk must still be buffered before shutdown"
    );

    if let Some(loss) = prior_ownership_loss {
        let input = match loss {
            "generation" => r#"execution_generation: "replacement-generation""#.to_owned(),
            "expired" => r#"execution_lease_expires_at: "2000-01-01T00:00:00Z""#.to_owned(),
            "completed" | "failed" | "dead" => {
                format!(
                    "lifecycle_state: \"{}\"",
                    crate::graphql::escape_graphql_string(loss)
                )
            }
            _ => panic!("unsupported ownership-loss fixture"),
        };
        let access = crate::config_client::ConfigAccess::Local(node.clone());
        access
            .write(
                "test.shutdown_prior_ownership_loss",
                &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, input: {{ {input} }}) {{ _docID }} }}"#),
            )
            .await
            .unwrap();
        let authority_query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ _docID lifecycle_state execution_generation execution_lease_expires_at terminal_output failure_reason }} }}"#
        );
        let authority_before = access.execute(&authority_query).await.unwrap();
        let authority = &authority_before["data"]["AgentRequest"][0];
        if loss == "generation" {
            assert_eq!(authority["lifecycle_state"], "processing");
            assert_eq!(authority["execution_generation"], "replacement-generation");
        } else if loss == "expired" {
            assert_eq!(authority["lifecycle_state"], "processing");
            assert!(
                chrono::DateTime::parse_from_rfc3339(
                    authority["execution_lease_expires_at"].as_str().unwrap()
                )
                .unwrap()
                    < chrono::Utc::now()
            );
        } else {
            assert_eq!(authority["lifecycle_state"], loss);
        }

        shutdown_tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(10), &mut process)
            .await
            .expect("already-lost execution must exit promptly")
            .expect("already-lost ownership is not a still-owned drain failure");
        assert_eq!(
            access.execute(&authority_query).await.unwrap(),
            authority_before,
            "old execution changed the authoritative request during shutdown"
        );
        let after = access.execute(&query).await.unwrap();
        let mut before_rows = before.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .clone();
        let mut after_rows = after["data"]["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .clone();
        let by_doc_id = |left: &serde_json::Value, right: &serde_json::Value| {
            left["_docID"].as_str().cmp(&right["_docID"].as_str())
        };
        before_rows.sort_by(by_doc_id);
        after_rows.sort_by(by_doc_id);
        assert_eq!(
            after_rows, before_rows,
            "old execution must neither retain its buffered chunk nor claim a new close"
        );
        node.shutdown().await;
        return;
    }

    if fail_drain {
        let (result, injected_writes) =
            crate::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
                Some(1),
                async {
                    shutdown_tx.send_replace(true);
                    tokio::time::timeout(Duration::from_secs(10), &mut process)
                        .await
                        .expect("failed drain must stop promptly")
                },
            )
            .await;
        assert_eq!(
            injected_writes, 1,
            "injected fault must reach the real canonical drain write"
        );
        let error = result.expect_err("failed drain must be reported to the daemon caller");
        assert!(
            error.is::<super::ShutdownDrainFailure>(),
            "unexpected failure: {error:#}"
        );
        let persisted = node.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state terminal_output }} }}"#)).await;
        assert!(!persisted.has_errors(), "{:?}", persisted.errors);
        let request = &persisted.data.as_ref().unwrap()["AgentRequest"][0];
        assert_eq!(
            request["lifecycle_state"], "processing",
            "uncertain drain must leave durable lease for recovery"
        );
        assert!(
            request["terminal_output"].is_null(),
            "failed drain must not claim saved terminal output"
        );
        let after = node.execute(&query).await;
        assert!(!after.has_errors(), "{:?}", after.errors);
        let mut before_rows = before.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .clone();
        let mut after_rows = after.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .clone();
        let by_doc_id = |left: &serde_json::Value, right: &serde_json::Value| {
            left["_docID"].as_str().cmp(&right["_docID"].as_str())
        };
        before_rows.sort_by(by_doc_id);
        after_rows.sort_by(by_doc_id);
        assert_eq!(
            after_rows, before_rows,
            "failed transaction must leave exact canonical segment facts unchanged"
        );
        let closed_ids = |rows: &[serde_json::Value]| {
            rows.iter()
                .filter(|row| {
                    matches!(
                        decode_output_segment_row(row).unwrap().segment.close,
                        Some(SourceClose::Closed { .. })
                    )
                })
                .map(|row| row["_docID"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            closed_ids(&after_rows),
            closed_ids(&before_rows),
            "failed drain must not add a claimed source close"
        );
        node.shutdown().await;
        return;
    }

    shutdown_tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(10), &mut process)
        .await
        .expect("graceful shutdown must finish")
        .expect("received reasoning drain must succeed");
    let after = node.execute(&query).await;
    assert!(!after.has_errors(), "{:?}", after.errors);
    let rows = after.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .collect::<Vec<_>>();
    let closes = rows
        .iter()
        .filter(|row| {
            matches!(
                row.segment.close,
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Partial,
                    ..
                })
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        closes.len(),
        1,
        "shutdown must close the exact received prefix once"
    );
    let facts = rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let Some(SourceClose::Closed { stream_bytes, .. }) = &closes[0].segment.close else {
        unreachable!()
    };
    let reconstructed = (0..stream_bytes.len())
        .map(|stream| {
            reconstruct_stream(
                &facts,
                &[],
                &[],
                &PayloadRef {
                    close_doc_id: closes[0].doc_id.clone(),
                    stream: u32::try_from(stream).unwrap(),
                },
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reconstructed.len(),
        2,
        "exact received part count: {reconstructed:?}"
    );
    for (part, text) in [(0, "first"), (1, " second")] {
        let content = reconstructed[part]
            .as_ref()
            .expect("closed reasoning stream reconstructs");
        assert_eq!(content.declaration.payload, StreamPayload::Reasoning);
        assert_eq!(content.declaration.block_index, 0);
        assert_eq!(content.declaration.part_index, u32::try_from(part).unwrap());
        assert_eq!(content.text, text);
    }
    node.shutdown().await;
}

#[tokio::test]
async fn graceful_shutdown_closes_all_received_buffered_reasoning() {
    graceful_shutdown_reasoning_case(false, None).await;
}

#[tokio::test]
async fn failed_graceful_shutdown_drain_reports_error_without_terminal_claim() {
    graceful_shutdown_reasoning_case(true, None).await;
}

#[tokio::test]
async fn graceful_shutdown_after_authoritative_terminal_does_not_claim_lost_output() {
    for state in ["completed", "failed", "dead"] {
        graceful_shutdown_reasoning_case(false, Some(state)).await;
    }
}

#[tokio::test]
async fn graceful_shutdown_after_generation_replacement_does_not_claim_lost_output() {
    graceful_shutdown_reasoning_case(false, Some("generation")).await;
}

#[tokio::test]
async fn graceful_shutdown_after_lease_expiry_does_not_claim_lost_output() {
    graceful_shutdown_reasoning_case(false, Some("expired")).await;
}

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
struct LeaseBlockingTool {
    entered: Option<Arc<AtomicBool>>,
}

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
        let entered = self.entered.clone();
        Box::pin(async move {
            if let Some(entered) = entered {
                entered.store(true, Ordering::SeqCst);
            }
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn lease_poll_ownership_loss_does_not_fail_a_running_tool() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let agent_config = test_agent();
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let mut daemon = AgentDaemon::new(
        node.clone(),
        agent_config.clone(),
        None,
        Arc::new(LeaseLossProvider),
        prompt.preamble().to_owned(),
        Arc::new(vec![
            Box::new(LeaseBlockingTool { entered: None }) as Box<dyn crate::llm::tool::ToolDyn>
        ]),
        prompt,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), node_did.clone()),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap();
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let request_doc_id = request.doc_id.clone();
    let session = gents_protocol::session::AgentSession {
        session_id: request.session_id.clone(),
        node_did: node_did.clone(),
        requester_did: request.requester_did.clone(),
        agent_id: agent_config.agent_id.clone(),
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
        (&mut process).await.expect("lease replacement handling completes");
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
            reasoning_prefix: false,
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
        if self.reasoning_prefix {
            let inner: rig::streaming::StreamingResult<()> =
                Box::pin(stream::unfold(0usize, |index| async move {
                    if index == 0 {
                        Some((
                            Ok(RawStreamingChoice::ReasoningDelta {
                                id: None,
                                reasoning: "received deadline reasoning".into(),
                            }),
                            1,
                        ))
                    } else {
                        std::future::pending().await
                    }
                }));
            return Ok(StreamingCompletionResponse::stream(inner));
        }
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
    let mut agent_config = test_agent();
    Arc::get_mut(&mut agent_config)
        .unwrap()
        .stream_liveness_timeout = Duration::from_secs(1);
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let model = NonTerminalProvider {
        empty_forever,
        active_chunks: None,
        reasoning_prefix: false,
        stream_calls: Arc::new(AtomicUsize::new(0)),
        empty_deltas: Arc::new(AtomicUsize::new(0)),
    };
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let mut daemon = AgentDaemon::new(
        node.clone(),
        agent_config.clone(),
        None,
        Arc::new(model.clone()),
        prompt.preamble().to_owned(),
        Arc::new(Vec::new()),
        prompt,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), node_did.clone()),
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
        let request = create_routed_request(&node, &agent_config, &node_did).await;
        // This test counts request inference attempts exactly. A supplied title
        // prevents optional background title generation from sharing the same
        // deliberately nonterminal provider and inflating calls/captures.
        let session = gents_protocol::session::AgentSession {
            session_id: request.session_id.clone(),
            node_did: node_did.clone(),
            requester_did: request.requester_did.clone(),
            agent_id: agent_config.agent_id.clone(),
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
                        let recovered = crate::RequestLifecycle::recover_all(&node, &node_did)
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
                            &node_did,
                            requester_did.as_deref(),
                        )
                        .await
                        .unwrap();
                    }
                };
                tokio::pin!(maintenance);
                tokio::select! {
                    result = &mut process => result.expect("nonterminal request handling completes"),
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
                        &node, &message_doc_id, &node_did, requester_did.as_deref(),
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
                        messages: &[], node_did: &node_did, requester_did: requester_did.as_deref(),
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
            let second = crate::RequestLifecycle::recover_all(&node, &node_did)
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
async fn daemon_interrupt_completion_preserves_wake_published_after_latch() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let agent_config = test_agent();
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let tool_entered = Arc::new(AtomicBool::new(false));
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let mut daemon = AgentDaemon::new(
        node.clone(),
        agent_config.clone(),
        None,
        Arc::new(LeaseLossProvider),
        prompt.preamble().to_owned(),
        Arc::new(vec![Box::new(LeaseBlockingTool {
            entered: Some(tool_entered.clone()),
        }) as Box<dyn crate::llm::tool::ToolDyn>]),
        prompt,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), node_did.clone()),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap();
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let request_doc_id = request.doc_id.clone();
    let session_id = request.session_id.clone();
    let requester_did = request.requester_did.clone();
    let session = gents_protocol::session::AgentSession {
        session_id: session_id.clone(),
        node_did: node_did.clone(),
        requester_did: requester_did.clone(),
        agent_id: agent_config.agent_id.clone(),
        created_at: request.created_at.clone(),
        closed_at: None,
        title: Some(gents_protocol::session::SessionTitle {
            text: "late wake interrupt regression".into(),
            source: gents_protocol::session::SessionTitleSource::Task,
        }),
        tags: vec![],
        provenance: None,
        observation: None,
    };
    let session_input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session).unwrap())
            .unwrap();
    access
        .transact("test.daemon_post_latch_session", |txn| {
            let session_input = &session_input;
            Box::pin(async move {
                txn.execute(&format!(
                    "mutation {{ create_AgentSession(input: {session_input}) {{_docID}} }}"
                ))
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let process = daemon.process_request(request, shutdown_rx);
    tokio::pin!(process);
    let escaped_request_doc = crate::graphql::escape_graphql_string(&request_doc_id);
    tokio::time::timeout(Duration::from_secs(10), async {
        let wait_for_blocked_tool = async {
            loop {
                let response = access
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_request_doc}" }} }}) {{ lifecycle_state }} AgentToolCall(filter: {{ tool_name: {{ _eq: "lease_block" }} }}) {{ lifecycle_state request_doc_id }} }}"#
                    ))
                    .await
                    .unwrap();
                if response["data"]["AgentRequest"][0]["lifecycle_state"]
                    == "processing"
                    && tool_entered.load(Ordering::SeqCst)
                    && response["data"]["AgentToolCall"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|row| {
                            row["lifecycle_state"] == "running"
                                && row["request_doc_id"] == request_doc_id
                        })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::pin!(wait_for_blocked_tool);
        tokio::select! {
            _ = &mut wait_for_blocked_tool => {}
            _ = &mut process => panic!("daemon exited before the accepted tool reached running"),
        }
    })
    .await
    .expect("accepted tool should block after its running state commits");

    crate::interrupt::interrupt_request_by_doc_id(
        &node,
        &request_doc_id,
        &node_did,
        requester_did.as_deref(),
    )
    .await
    .unwrap();
    let wake_id = "daemon-post-latch-wake";
    let wake_input = serde_json::json!({
        "request_id": wake_id,
        "node_did": node_did.clone(),
        "requester_did": requester_did.clone(),
        "session_id": session_id.clone(),
        "agent_id": agent_config.agent_id.clone(),
        "lifecycle_state": "pending",
        "execution_origin": "scheduled",
        "input": {"queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": format!("background_completion:{session_id}"),
        }},
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    crate::config_client::ConfigAccess::transact_local(
        &node,
        None,
        "test.daemon_post_latch_wake",
        |txn| {
            let wake_input = wake_input.clone();
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &serde_json::json!({"input": wake_input}),
                )
                .await?;
                Ok(())
            })
        },
    )
    .await
    .unwrap();

    tokio::time::timeout(Duration::from_secs(10), &mut process)
        .await
        .expect("interrupted daemon request should terminalize")
        .expect("interrupted request handling completes");
    let wake_id = crate::graphql::escape_graphql_string(wake_id);
    let result = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{wake_id}" }} }}) {{ lifecycle_state }} AgentRequestParent: AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_request_doc}" }} }}) {{ lifecycle_state terminal_output }} }}"#
        ))
        .await
        .unwrap();
    assert_eq!(
        result["data"]["AgentRequest"][0]["lifecycle_state"],
        "pending"
    );
    assert_eq!(
        result["data"]["AgentRequestParent"][0]["lifecycle_state"],
        "interrupted"
    );
    assert!(!result["data"]["AgentRequestParent"][0]["terminal_output"].is_null());
    node.shutdown().await;
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
    let mut agent_config = test_agent();
    {
        let agent_config = Arc::get_mut(&mut agent_config).unwrap();
        agent_config.stream_batch_ms = crate::config::DEFAULT_STREAM_BATCH_MS;
        agent_config.stream_liveness_timeout = Duration::from_secs(1);
    }
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let model = NonTerminalProvider {
        empty_forever: false,
        active_chunks: Some(8),
        reasoning_prefix: false,
        stream_calls: Arc::new(AtomicUsize::new(0)),
        empty_deltas: Arc::new(AtomicUsize::new(0)),
    };
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let mut daemon = AgentDaemon::new(
        node.clone(),
        agent_config.clone(),
        None,
        Arc::new(model.clone()),
        prompt.preamble().to_owned(),
        Arc::new(Vec::new()),
        prompt,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), node_did.clone()),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let request_id = request.request_id.clone();
    let request_doc_id = request.doc_id.clone();
    let requester_did = request.requester_did.clone();
    let session = gents_protocol::session::AgentSession {
        session_id: request.session_id.clone(),
        node_did: node_did.clone(),
        requester_did: request.requester_did.clone(),
        agent_id: agent_config.agent_id.clone(),
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
    .expect("active stream completes across multiple lease durations")
    .expect("active request handling completes");

    let request_id = crate::graphql::escape_graphql_string(&request_id);
    let result = node
        .execute(&format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ lifecycle_state terminal_output }}
            }}"#
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let data = result.data.unwrap();
    assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "completed");
    let selected: gents_protocol::output::TerminalOutput =
        serde_json::from_value(data["AgentRequest"][0]["terminal_output"].clone()).unwrap();
    let gents_protocol::output::TerminalOutput::Message { message_doc_id } = selected else {
        panic!("completed streaming request must select its canonical message: {data}");
    };
    let (header, message) = crate::session::load_canonical_message_from_node(
        &node,
        &message_doc_id,
        &node_did,
        requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(
        header.request_doc_id.as_deref(),
        Some(request_doc_id.as_str())
    );
    assert_eq!(
        header.outcome,
        gents_protocol::output::OutputOutcome::Complete
    );
    let content = gents_protocol::transcript::present_message(&message).body_markdown;
    assert!(
        content.contains("chunk-8"),
        "final output was truncated: {data}"
    );
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);
    node.shutdown().await;
}

#[tokio::test]
async fn deadline_closes_received_openai_reasoning_without_another_provider_call() {
    use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
    use gents_protocol::output::{OutputOutcome, PayloadRef, SourceClose};

    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let mut agent_config = test_agent();
    {
        let agent_config = Arc::get_mut(&mut agent_config).unwrap();
        agent_config.deadline_duration = Duration::from_secs(3);
        agent_config.stream_liveness_timeout = Duration::from_secs(8);
        agent_config.provider_idle_timeout = Duration::from_secs(8);
    }
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let model = NonTerminalProvider {
        empty_forever: false,
        active_chunks: None,
        reasoning_prefix: true,
        stream_calls: Arc::new(AtomicUsize::new(0)),
        empty_deltas: Arc::new(AtomicUsize::new(0)),
    };
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let mut daemon = AgentDaemon::new(
        node.clone(),
        agent_config.clone(),
        None,
        Arc::new(model.clone()),
        prompt.preamble().to_owned(),
        Arc::new(Vec::new()),
        prompt,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), node_did.clone()),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap();
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let doc_id = request.doc_id.clone();
    let session = gents_protocol::session::AgentSession {
        session_id: request.session_id.clone(),
        node_did: node_did.clone(),
        requester_did: request.requester_did.clone(),
        agent_id: agent_config.agent_id.clone(),
        created_at: request.created_at.clone(),
        closed_at: None,
        title: Some(gents_protocol::session::SessionTitle {
            text: "deadline reasoning regression".into(),
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
    tokio::time::timeout(
        Duration::from_secs(10),
        daemon.process_request(request, shutdown_rx),
    )
    .await
    .expect("request deadline must stop the nonterminal provider")
    .expect("deadline request handling completes");

    let escaped = crate::graphql::escape_graphql_string(&doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert_eq!(
        response.data.as_ref().unwrap()["AgentRequest"][0]["lifecycle_state"],
        "failed"
    );
    let result = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let rows = result.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .collect::<Vec<_>>();
    let closes = rows
        .iter()
        .filter(|row| {
            matches!(
                row.segment.close,
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Partial,
                    ..
                })
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        closes.len(),
        1,
        "deadline must close the owned received prefix once"
    );
    let facts = rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let Some(SourceClose::Closed { stream_bytes, .. }) = &closes[0].segment.close else {
        unreachable!("filtered partial close")
    };
    assert!((0..stream_bytes.len()).any(|stream| {
        reconstruct_stream(
            &facts,
            &[],
            &[],
            &PayloadRef {
                close_doc_id: closes[0].doc_id.clone(),
                stream: u32::try_from(stream).unwrap(),
            },
        )
        .is_ok_and(|content| content.text == "received deadline reasoning")
    }));
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);
    node.shutdown().await;
}
