use super::*;

pub(super) enum ScriptedCall {
    Turn(Vec<RawStreamingChoice<()>>),
    FailStream(CompletionError),
    TurnWithMidStreamError(Vec<RawStreamingChoice<()>>, CompletionError),
}

/// A `CompletionModel` whose `stream` replays one scripted call: each
/// `stream()` pops the next [`ScriptedCall`] from the queue, letting a test
/// drive multi-turn loops and provider failures without a provider. Once the
/// queue is empty it yields a bare final response so the loop terminates.
#[derive(Clone)]
pub(super) struct ScriptedModel {
    pub(super) calls: Arc<Mutex<VecDeque<ScriptedCall>>>,
    /// `chat_history` of every request the loop sent, in order (converted to
    /// native at the capture boundary) — lets a test assert how the loop
    /// threaded prior turns back to the provider.
    seen_histories: Arc<Mutex<Vec<Vec<Message>>>>,
    /// Advertised tool names (`request.tools`) of every request the loop sent,
    /// in order — lets a test assert the toolset is attached on every turn.
    seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
    /// Effective per-turn output caps after the provider-input budget clamp.
    seen_max_tokens: Arc<Mutex<Vec<Option<u64>>>>,
    /// Exact request snapshots consumed by the mock transport.
    seen_requests: Arc<Mutex<Vec<CompletionRequest>>>,
    /// When set, every turn's stream yields its scripted chunks then hangs
    /// (never reaches EOF), simulating a provider that stalls mid-turn.
    stall_after_chunks: bool,
}

impl ScriptedModel {
    pub(super) fn new(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self::new_turns(vec![chunks])
    }

    pub(super) fn new_turns(turns: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self::new_calls(turns.into_iter().map(ScriptedCall::Turn).collect())
    }

    pub(super) fn new_calls(calls: Vec<ScriptedCall>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(calls.into())),
            seen_histories: Arc::new(Mutex::new(Vec::new())),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            seen_max_tokens: Arc::new(Mutex::new(Vec::new())),
            seen_requests: Arc::new(Mutex::new(Vec::new())),
            stall_after_chunks: false,
        }
    }

    /// A single turn that emits `chunks` then stalls forever instead of ending.
    pub(super) fn new_stalling(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        let mut model = Self::new_calls(vec![ScriptedCall::Turn(chunks)]);
        model.stall_after_chunks = true;
        model
    }

    pub(super) async fn seen_histories(&self) -> Vec<Vec<Message>> {
        self.seen_histories.lock().await.clone()
    }

    pub(super) async fn seen_tools(&self) -> Vec<Vec<String>> {
        self.seen_tools.lock().await.clone()
    }

    pub(super) async fn seen_max_tokens(&self) -> Vec<Option<u64>> {
        self.seen_max_tokens.lock().await.clone()
    }

    pub(super) async fn seen_requests(&self) -> Vec<CompletionRequest> {
        self.seen_requests.lock().await.clone()
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for ScriptedModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self::new_turns(Vec::new())
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in loop_stream tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        self.seen_requests.lock().await.push(request.clone());
        self.seen_histories.lock().await.push(
            request
                .chat_history
                .iter()
                .map(crate::llm::rig_compat::from_rig_message)
                .collect(),
        );
        self.seen_tools
            .lock()
            .await
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        self.seen_max_tokens.lock().await.push(request.max_tokens);
        let call = self
            .calls
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| ScriptedCall::Turn(vec![RawStreamingChoice::FinalResponse(())]));
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> = match call {
            ScriptedCall::Turn(chunks) => chunks.into_iter().map(Ok).collect(),
            ScriptedCall::FailStream(error) => return Err(error),
            ScriptedCall::TurnWithMidStreamError(chunks, error) => {
                let mut items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
                    chunks.into_iter().map(Ok).collect();
                items.push(Err(error));
                items
            }
        };
        let inner: rig::streaming::StreamingResult<()> = if self.stall_after_chunks {
            Box::pin(stream::iter(items).chain(stream::pending()))
        } else {
            Box::pin(stream::iter(items))
        };
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct UsageResponse {
    pub(super) usage: Option<Usage>,
}

impl GetTokenUsage for UsageResponse {
    fn token_usage(&self) -> Option<Usage> {
        self.usage
    }
}

pub(super) fn usage_response(
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
) -> UsageResponse {
    UsageResponse {
        usage: Some(Usage {
            input_tokens,
            output_tokens,
            total_tokens,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }),
    }
}

/// A usage-reporting provider double for request-wide budget behavior. It
/// records the fully rendered input estimate and effective output ceiling at
/// the same boundary where a real provider receives them.
#[derive(Clone)]
pub(super) struct UsageScriptedModel {
    turns: Arc<Mutex<VecDeque<Vec<RawStreamingChoice<UsageResponse>>>>>,
    seen_dispatches: Arc<Mutex<Vec<(u64, Option<u64>)>>>,
}

impl UsageScriptedModel {
    pub(super) fn new(turns: Vec<Vec<RawStreamingChoice<UsageResponse>>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            seen_dispatches: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) async fn seen_dispatches(&self) -> Vec<(u64, Option<u64>)> {
        self.seen_dispatches.lock().await.clone()
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for UsageScriptedModel {
    type Response = ();
    type StreamingResponse = UsageResponse;
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self::new(Vec::new())
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in loop_stream tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        self.seen_dispatches.lock().await.push((
            u64::try_from(
                completion_request_input_components(
                    &request,
                    &crate::provider_input::ProviderInputCounter::new(
                        crate::BackendProviderKind::OpenAiCompatible,
                        crate::OpenAiWireApi::ChatCompletions,
                        "test-model",
                    ),
                )
                .map(|projection| projection.estimated_input_tokens)
                .unwrap_or(usize::MAX),
            )
            .unwrap_or(u64::MAX),
            request.max_tokens,
        ));
        let turn = self.turns.lock().await.pop_front().unwrap_or_else(|| {
            vec![RawStreamingChoice::FinalResponse(UsageResponse {
                usage: None,
            })]
        });
        let inner: rig::streaming::StreamingResult<UsageResponse> =
            Box::pin(stream::iter(turn.into_iter().map(Ok)));
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

/// A trivial tool that echoes a fixed output, for dispatch tests.
pub(super) struct EchoTool {
    pub(super) name: String,
    pub(super) output: String,
}

impl ToolDyn for EchoTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "echo".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.output.clone()) })
    }
}

pub(super) struct CountingTool {
    pub(super) name: String,
    pub(super) output: String,
    pub(super) calls: Arc<AtomicUsize>,
}

impl ToolDyn for CountingTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "counting".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        let calls = self.calls.clone();
        let output = self.output.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(output)
        })
    }
}

/// A tool whose call always returns a fixed string (used for large/managed
/// outputs); name defaults to "echo".
pub(super) struct FixedTool {
    pub(super) name: String,
    pub(super) output: String,
}

impl ToolDyn for FixedTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "fixed".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.output.clone()) })
    }
}

/// Records the prompt/rag string handed to `definition`, for the rag-text test.
pub(super) struct RecordingDefinitionTool {
    pub(super) seen_prompt: Arc<Mutex<Option<String>>>,
}

impl ToolDyn for RecordingDefinitionTool {
    fn name(&self) -> String {
        "record".to_string()
    }

    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition> {
        let seen = self.seen_prompt.clone();
        Box::pin(async move {
            *seen.lock().await = Some(prompt);
            ToolDefinition {
                name: "record".to_string(),
                description: "record".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok("ok".to_string()) })
    }
}

pub(super) fn echo_tool() -> Box<dyn ToolDyn> {
    Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })
}

/// Script one tool-calling turn that invokes `echo`.
pub(super) fn echo_tool_turn() -> Vec<RawStreamingChoice<()>> {
    vec![
        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        )),
        RawStreamingChoice::FinalResponse(()),
    ]
}

pub(super) fn usage_echo_tool_turn(
    response: UsageResponse,
) -> Vec<RawStreamingChoice<UsageResponse>> {
    vec![
        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        )),
        RawStreamingChoice::FinalResponse(response),
    ]
}

pub(super) fn tool_result_text(content: &ToolResultContent) -> &str {
    match content {
        ToolResultContent::Text(text) => text.text.as_str(),
        ToolResultContent::Image(_) => "",
    }
}

pub(super) fn config(max_turns: usize) -> LoopConfig {
    LoopConfig {
        provider_input_counter: std::sync::Arc::new(
            crate::provider_input::ProviderInputCounter::new(
                crate::BackendProviderKind::OpenAiCompatible,
                crate::OpenAiWireApi::ChatCompletions,
                "test-model",
            ),
        ),
        preamble: None,
        context_message: None,
        temperature: None,
        max_tokens: None,
        aggregate_token_budget: None,
        additional_params: None,
        structured_output: None,
        tool_choice: None,
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
        compaction_threshold: crate::config::DEFAULT_COMPACTION_THRESHOLD,
        retry_policy: crate::agent::completion_retry::CompletionRetryPolicy::scheduled_default(),
        deadline: None,
        max_turns,
        output_obligation_gate: None,
    }
}

#[derive(Debug)]
pub(super) struct AttemptEvent {
    pub(super) turn: usize,
    pub(super) attempt: u32,
    pub(super) will_retry: bool,
    pub(super) backoff: Duration,
}

#[derive(Debug, Default)]
pub(super) struct CollectedScriptedStream {
    pub(super) attempts: Vec<AttemptEvent>,
    pub(super) text_chunks: Vec<String>,
    pub(super) tool_results: Vec<String>,
    pub(super) retractions: Vec<(usize, u32)>,
    pub(super) final_text: Option<String>,
    pub(super) error: Option<String>,
}

pub(super) async fn collect_scripted_stream<S, R>(stream: S) -> CollectedScriptedStream
where
    S: Stream<Item = Result<LoopStreamItem<R>, StreamingError>>,
{
    futures::pin_mut!(stream);
    let mut collected = CollectedScriptedStream::default();

    loop {
        match tokio::time::timeout(Duration::from_millis(1), stream.next()).await {
            Ok(Some(Ok(LoopStreamItem::AttemptFailed {
                turn,
                attempt,
                error: _,
                will_retry,
                backoff,
            }))) => collected.attempts.push(AttemptEvent {
                turn,
                attempt,
                will_retry,
                backoff,
            }),
            Ok(Some(Ok(LoopStreamItem::TurnRetracted { turn, attempt, .. }))) => {
                collected.retractions.push((turn, attempt));
            }
            Ok(Some(Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Text(text),
            ))))) => {
                collected.text_chunks.push(text.text);
            }
            Ok(Some(Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
                StreamedUserContent::ToolResult { tool_result, .. },
            ))))) => {
                collected.tool_results.push(
                    tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                        &tool_result.content.first(),
                    ))
                    .to_string(),
                );
            }
            Ok(Some(Ok(LoopStreamItem::Item(MultiTurnStreamItem::FinalResponse(
                final_response,
            ))))) => {
                collected.final_text = Some(final_response.response().to_string());
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(error))) => {
                collected.error = Some(error.to_string());
                break;
            }
            Ok(None) => break,
            Err(_) => {
                tokio::time::advance(Duration::from_secs(300)).await;
            }
        }
    }

    collected
}

pub(super) fn transient_provider_error(label: &str) -> CompletionError {
    CompletionError::ProviderError(format!("status code 503: {label}"))
}

pub(super) fn permanent_provider_error() -> CompletionError {
    CompletionError::ProviderError("status code 400: duplicate field max_tokens".to_string())
}

pub(super) fn parse_400_text(tag: &str) -> String {
    format!("BadRequestError: Expecting value [{tag}]: line 1 column 28 (char 27)")
}

pub(super) fn parse_400_error(tag: &str) -> CompletionError {
    CompletionError::ProviderError(parse_400_text(tag))
}

pub(super) fn assert_duration_in_range(delay: Duration, low_ms: u64, high_ms: u64) {
    let actual_ms = delay.as_millis() as u64;
    assert!(
        actual_ms >= low_ms && actual_ms <= high_ms,
        "expected duration in [{low_ms}, {high_ms}]ms, got {actual_ms}ms"
    );
}

pub(super) fn history_has_control_char_tool_arg(history: &[Message]) -> bool {
    history.iter().any(|message| match message {
        Message::Assistant { content, .. } => content.iter().any(|item| match item {
            AssistantContent::ToolCall(tool_call) => {
                json_value_has_control_char(&tool_call.function.arguments)
            }
            _ => false,
        }),
        _ => false,
    })
}

pub(super) fn history_has_tool_call(history: &[Message], tool_name: &str) -> bool {
    history.iter().any(|message| match message {
        Message::Assistant { content, .. } => content.iter().any(|item| {
            matches!(
                item,
                AssistantContent::ToolCall(tool_call)
                    if tool_call.function.name == tool_name
            )
        }),
        _ => false,
    })
}

pub(super) fn history_has_tool_result_text(history: &[Message], expected: &str) -> bool {
    history.iter().any(|message| match message {
        Message::User { content } => content.iter().any(|item| {
            matches!(
                item,
                UserContent::ToolResult(result)
                    if tool_result_text(first_content(&result.content)) == expected
            )
        }),
        _ => false,
    })
}

pub(super) fn json_value_has_control_char(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text.chars().any(char::is_control),
        serde_json::Value::Array(values) => values.iter().any(json_value_has_control_char),
        serde_json::Value::Object(map) => map.values().any(json_value_has_control_char),
        _ => false,
    }
}

pub(super) async fn test_hook() -> (Arc<defra_node::EmbeddedNode>, DefraSessionHook) {
    let data_path =
        std::env::temp_dir().join(format!("agent-loop-stream-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();
    let session_id = uuid::Uuid::new_v4().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let deadline_at = now + chrono::Duration::seconds(60);
    let mutation = format!(
        r#"mutation {{
            create_AgentSession(input: {{
                session_id: "{session_id}",
                agent_name: "general",
                agent_did: "did:test:test",
                behavior_id: "general",
                started: "{now}",
                status: "active"
            }}) {{ _docID }}
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "did:test:test",
                behavior_id: "general",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "loop test request",
                status: "processing",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "user",
                created_at: "{now}",
                valid_until: "{deadline_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        now = now.to_rfc3339(),
        deadline_at = deadline_at.to_rfc3339(),
        max_retries = crate::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create loop test request context failed: {:?}",
        response.errors
    );
    let hook = DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        &session_id,
        "general",
        "did:test:test",
        FailurePolicy::default(),
    )
    .await
    .expect("resume loop test session");
    hook.set_active_request_lineage(Some(request_id), None)
        .await
        .expect("bind persisted loop test request");
    hook.set_request_deadline_at(Some(deadline_at)).await;
    (node, hook)
}
