use super::*;
use rig::completion::{CompletionError, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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
