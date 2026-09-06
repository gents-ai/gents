use super::*;
use rig::completion::{CompletionError, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

async fn capture_scripted_request(
    request: CompletionRequest,
) -> Result<serde_json::Value, CompletionError> {
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
    let transport =
        crate::rendered_request::transport::RenderedRequestCapturingHttpClient::new(inner.clone());
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

    Ok(body)
}

#[derive(Clone)]
struct PartialThenEmptyProvider(Arc<AtomicUsize>);

#[allow(refining_impl_trait)]
impl CompletionModel for PartialThenEmptyProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("streaming only".into()))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        capture_scripted_request(request).await?;

        let observations = self.0.clone();
        let inner: rig::streaming::StreamingResult<()> =
            Box::pin(futures::stream::unfold(false, move |started| {
                let observations = observations.clone();
                async move {
                    let text = if started {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        observations.fetch_add(1, Ordering::SeqCst);
                        String::new()
                    } else {
                        "durable one-shot partial".to_string()
                    };
                    Some((Ok(RawStreamingChoice::Message(text)), true))
                }
            }));
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

#[tokio::test]
async fn oneshot_honors_short_semantic_lease_and_recovers_partial_empty_stream_before_return() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(dir.path().join("identity.key"), None)
            .unwrap();
    let mut behavior = crate::agent::PendingAgentBehavior::new("oneshot-lease")
        .build_with_identity_for_test(identity);
    behavior.model_name = "scripted".to_owned();
    behavior.stream_liveness_timeout = Duration::from_secs(1);
    behavior.deadline_duration = Duration::from_secs(60);
    let prompt = LayeredPromptBuilder::for_behavior(
        &behavior.system_prompt,
        &behavior.behavior_id,
        &[],
        false,
        &[],
    );
    let config = loop_config(
        &behavior,
        prompt.preamble().to_owned(),
        0,
        crate::rendered_request::CaptureScopeKind::OneShot,
    );
    let observations = Arc::new(AtomicUsize::new(0));
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run_oneshot_owned(
            node.clone(),
            &behavior,
            &prompt,
            PartialThenEmptyProvider(observations.clone()),
            "exercise semantic timeout",
            Arc::new(Vec::new()),
            config,
            &[],
            BackgroundToolRegistry::default(),
            crate::toolset::lsp::LspPool::new(),
        ),
    )
    .await
    .expect("one-shot must use its 1s semantic lease rather than the default 30-minute lease");
    assert!(
        result.is_err(),
        "an endless stream cannot complete successfully"
    );
    assert!(
        observations.load(Ordering::SeqCst) > 0,
        "provider must send traffic after semantic progress stops; one-shot returned {result:?}"
    );
    // No test-driven recovery: the one-shot owner must converge before return.
    let result = node
        .execute(
            r#"{
        AgentRequest { lifecycle_state execution_progress_seq }
        AgentResponse { status content }
        RenderedRequest { _docID }
        AgentMessage(filter: { role: { _eq: "assistant" } }) { content }
    }"#,
        )
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let data = result.data.as_ref().unwrap();
    assert_eq!(
        data["RenderedRequest"].as_array().unwrap().len(),
        1,
        "the fake provider request must be durably captured before streaming"
    );
    assert_eq!(data["AgentRequest"].as_array().unwrap().len(), 1);
    assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "failed");
    assert_eq!(data["AgentResponse"].as_array().unwrap().len(), 1);
    assert_eq!(data["AgentResponse"][0]["status"], "error");
    assert!(
        data["AgentResponse"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("durable one-shot partial")
            || data["AgentMessage"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("durable one-shot partial")),
        "partial progress must survive convergence: {data}"
    );
    let repeated = RequestLifecycle::recover_all(&node, behavior.agent_did())
        .await
        .unwrap();
    assert_eq!(repeated.requests_recovered, 0);
    assert_eq!(repeated.responses_recovered, 0);
    node.shutdown().await;
}

#[derive(Clone)]
struct OutputObligationProvider {
    calls: Arc<AtomicUsize>,
    node: Arc<EmbeddedNode>,
}

#[allow(refining_impl_trait)]
impl CompletionModel for OutputObligationProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        panic!("construct scripted output-obligation provider with its fixture node")
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("streaming only".into()))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        let body = capture_scripted_request(request).await?;

        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match turn {
            0 => vec![
                RawStreamingChoice::Message("premature final".into()),
                RawStreamingChoice::FinalResponse(()),
            ],
            1 => {
                assert!(body.to_string().contains("write_oneshot_result"));
                assert!(body
                    .to_string()
                    .contains("configured output obligation is unmet"));
                let state = self
                    .node
                    .execute("{ AgentRequest { lifecycle_state } OneshotOutput { message } }")
                    .await;
                assert!(!state.has_errors(), "{:?}", state.errors);
                let state = state.data.unwrap();
                assert_eq!(state["AgentRequest"][0]["lifecycle_state"], "processing");
                assert!(state["OneshotOutput"].as_array().unwrap().is_empty());
                vec![
                    RawStreamingChoice::ToolCall(rig::streaming::RawStreamingToolCall::new(
                        "required-oneshot-write".into(),
                        "write_oneshot_result".into(),
                        serde_json::json!({"message": "durable output"}),
                    )),
                    RawStreamingChoice::FinalResponse(()),
                ]
            }
            2 => vec![
                RawStreamingChoice::Message("output complete".into()),
                RawStreamingChoice::FinalResponse(()),
            ],
            other => panic!("unexpected extra provider turn {other}"),
        };
        let stream: rig::streaming::StreamingResult<()> =
            Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)));
        Ok(StreamingCompletionResponse::stream(stream))
    }
}

#[tokio::test]
async fn oneshot_configured_output_gate_requires_real_write_and_respects_trigger_scope() {
    use crate::document_config::{
        WriteToolDecl, WriteToolField, WriteToolOutputObligation, WriteToolOutputObligationScope,
    };
    use crate::tool_surface::{BehaviorToolConfig, ToolCeiling, ToolRuntimeContext, ToolSelection};

    for request_scoped in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        node.add_schema("type OneshotOutput { message: String }")
            .await
            .unwrap();
        let identity =
            crate::identity::KeyIdentity::load_or_create(dir.path().join("identity.key"), None)
                .unwrap();
        let mut behavior = crate::agent::PendingAgentBehavior::new("oneshot-output")
            .build_with_identity_for_test(identity);
        behavior.model_name = "scripted".into();
        behavior.stream_liveness_timeout = Duration::from_secs(60);
        behavior.deadline_duration = Duration::from_secs(120);
        let mut selection = ToolSelection::default();
        selection.write_tools = vec![WriteToolDecl {
            tool_name: "write_oneshot_result".into(),
            collection: "OneshotOutput".into(),
            description: "Persist the required one-shot output.".into(),
            fields: vec![WriteToolField {
                name: "message".into(),
                required: true,
                fill: None,
            }],
            output_obligation: Some(WriteToolOutputObligation {
                scope: if request_scoped {
                    WriteToolOutputObligationScope::Request
                } else {
                    WriteToolOutputObligationScope::Trigger
                },
                minimum_writes: 1,
                expected_count_field: None,
            }),
        }];
        behavior.tools = BehaviorToolConfig::from_selection(
            &behavior.behavior_id,
            selection,
            &ToolCeiling::readwrite(dir.path()),
            Vec::new(),
        )
        .unwrap();
        let surface = behavior.tools.resolve(&node).await.unwrap();
        let obligations = surface.output_obligations();
        assert_eq!(
            obligations.len(),
            1,
            "configured writer contract must survive surface resolution"
        );
        let runtime =
            ToolRuntimeContext::oneshot_with_agent_did(node.clone(), behavior.agent_did());
        let tools = Arc::new(surface.build_tools(&runtime).unwrap());
        let prompt = LayeredPromptBuilder::new(&behavior, &surface, &[]);
        let mut config = loop_config(
            &behavior,
            prompt.preamble().to_owned(),
            tools.len(),
            crate::rendered_request::CaptureScopeKind::OneShot,
        );
        config.max_turns = 4;
        let calls = Arc::new(AtomicUsize::new(0));
        let result = run_oneshot_owned(
            node.clone(),
            &behavior,
            &prompt,
            OutputObligationProvider {
                calls: calls.clone(),
                node: node.clone(),
            },
            "finish this request",
            tools,
            config,
            &obligations,
            BackgroundToolRegistry::default(),
            runtime.lsp_pool.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if request_scoped { 3 } else { 1 }
        );
        assert_eq!(
            result.response_text,
            if request_scoped {
                "output complete"
            } else {
                "premature final"
            }
        );
        let persisted = node.execute(r#"{
            AgentRequest { _docID lifecycle_state }
            OneshotOutput { message }
            AgentToolCall(filter: { tool_name: { _eq: "write_oneshot_result" } }) { lifecycle_state request_doc_id }
            RenderedRequest { _docID }
        }"#).await;
        assert!(!persisted.has_errors(), "{:?}", persisted.errors);
        let data = persisted.data.unwrap();
        assert_eq!(data["AgentRequest"].as_array().unwrap().len(), 1);
        assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "completed");
        assert_eq!(
            data["OneshotOutput"].as_array().unwrap().len(),
            usize::from(request_scoped)
        );
        assert_eq!(
            data["AgentToolCall"].as_array().unwrap().len(),
            usize::from(request_scoped)
        );
        assert_eq!(
            data["RenderedRequest"].as_array().unwrap().len(),
            calls.load(Ordering::SeqCst)
        );
        if request_scoped {
            assert_eq!(data["OneshotOutput"][0]["message"], "durable output");
            assert_eq!(data["AgentToolCall"][0]["lifecycle_state"], "completed");
            assert_eq!(
                data["AgentToolCall"][0]["request_doc_id"],
                data["AgentRequest"][0]["_docID"]
            );
        }
        node.shutdown().await;
    }
}
