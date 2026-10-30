use super::*;

#[derive(Clone)]
pub(super) struct CountingReplyModel(Arc<AtomicUsize>);

#[allow(refining_impl_trait)]
impl CompletionModel for CountingReplyModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("unused".into()))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(vec![
            Ok(RawStreamingChoice::Message("admitted reply".to_string())),
            Ok(RawStreamingChoice::FinalResponse(())),
        ]));
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

pub(super) fn test_agent(identity: Arc<dyn NodeIdentity>) -> Arc<ResolvedAgent> {
    let principal = Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_id: "behavior-1".to_string(),
        display_name: None,
        enabled: true,
    });
    Arc::new(ResolvedAgent {
        profile_id: None,
        agent_id: "behavior-1".to_string(),
        node: principal,
        backend_id: Some("backend-behavior-1".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
        backend_auth: crate::documents::BackendAuth::Unauthenticated,
        model_name: "scripted".to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: 8_192,
        max_output_tokens: 1_024,
        max_turns: 2,
        max_turns_provenance: crate::inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: "system".to_string(),
        tools: AgentToolSurfaceConfig::meta_only(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: 0,
        stream_liveness_timeout: Duration::from_secs(5),
        deadline_duration: Duration::from_secs(30),
        provider_idle_timeout: Duration::from_secs(
            crate::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry: CompletionRetryProfileFields::default(),
        sampling: SamplingConfig::default(),
        skills: Vec::new(),
    })
}

pub(super) fn behavior_daemon(
    node: Arc<EmbeddedNode>,
    behavior: Arc<ResolvedAgent>,
    authority: crate::agent::p2p_reconcile::EnrollmentAuthorityHandle,
    calls: Arc<AtomicUsize>,
) -> AgentDaemon<CountingReplyModel> {
    let prompt_builder = LayeredPromptBuilder::for_agent(
        &behavior.system_prompt,
        &behavior.agent_id,
        &[],
        false,
        &[],
    );
    let runtime_status = crate::runtime_status::RuntimeStatusHandle::new(
        node.clone(),
        behavior.node_did().to_string(),
    );
    AgentDaemon::new(AgentDaemonInit {
        node: node.clone(),
        agent_config: behavior.clone(),
        provider_family: None,
        model: Arc::new(CountingReplyModel(calls)),
        preamble: prompt_builder.preamble().to_string(),
        loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
        prompt_builder,
        hook_failure_policy: FailurePolicy::default(),
        rendered_request_capture_factory: None,
        background_tool_registry: BackgroundToolRegistry::default(),
        background_execution_registry: BackgroundExecutionRegistry::default(),
        startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
        runtime_status,
        slot_generation: 1,
        request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
            node,
            behavior.node_identity().clone(),
            authority,
        ),
    })
    .expect("valid canonical daemon fixture")
}

pub(super) async fn seed_peer_session_message(
    node: &Arc<EmbeddedNode>,
    lifecycle: &crate::request::lifecycle::RequestLifecycle,
    writer: &crate::request::streaming::DefraStreamWriter,
    host_did: &str,
    tool_call_id: &str,
    prompt: &str,
    turn: usize,
) -> String {
    use crate::tool_call_lifecycle::{AwaitMode, ToolCallLifecycle};
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    let message = Message::Assistant {
        id: Some(format!("remote-provider-{turn}")),
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: tool_call_id.into(),
            call_id: Some(tool_call_id.into()),
            function: ToolFunction::new(
                crate::toolset::AGENT_NEW_TOOL_NAME.into(),
                serde_json::json!({ "agent": "remote-target", "prompt": prompt }),
            ),
            signature: None,
            additional_params: None,
        })],
    };
    let publication = writer
        .publish_native_turn(lifecycle, turn, 0, &message)
        .await
        .expect("accept remote agent_new intent");
    let accepted = publication.accepted_tools.into_iter().next().unwrap();
    let mut row = ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        accepted,
        lifecycle.claimed_deadline_at().unwrap(),
        AwaitMode::Background,
    )
    .expect("adopt accepted agent_new");
    let cause = crate::request::lifecycle::SessionMessageCause {
        caller_node_did: lifecycle.request().node_did.clone(),
        caller_agent_id: lifecycle.request().agent_id.clone(),
        caller_session_id: lifecycle.request().session_id.clone(),
        caller_requester_did: lifecycle.request().requester_did.clone(),
        caller_request_id: lifecycle.request().request_id.clone(),
        caller_request_doc_id: lifecycle.request().doc_id.clone(),
        caller_hop: lifecycle.request().request_hop,
        tool_call_id: row.tool_call_id().to_owned(),
        tool_call_doc_id: row.doc_id().unwrap().to_owned(),
        correlation: None,
    };
    let target = crate::request::lifecycle::SessionMessageTarget {
        node_did: host_did.to_owned(),
        agent_id: "behavior-1".to_owned(),
        session_id: uuid::Uuid::new_v4().to_string(),
    };
    let plan = crate::session_message::plan(
        node,
        &cause,
        &target,
        crate::session_message::RenderedBody {
            content: prompt.to_owned(),
            goal: None,
        },
        None,
        false,
    )
    .await
    .expect("plan peer session message")
    .expect("peer session message admitted by the caller");
    crate::session_message::commit(node, &cause, &mut row, plan, false)
        .await
        .expect("materialize the caller-signed Peer request")
        .request_id
}

pub(super) async fn load_admission_request_by_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> crate::request::watcher::AgentRequest {
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
            escape_graphql_string(request_id),
        ))
        .await;
    let doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("child doc id");
    crate::request::admission::load_request_for_admission_test(node, doc_id)
        .await
        .expect("load child")
}
