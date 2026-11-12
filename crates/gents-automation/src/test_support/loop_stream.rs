use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{stream, Stream, StreamExt};
use gents_inference::llm::message::{AssistantContent, ToolResultContent, UserContent};
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};

use gents_inference::llm::message::Message;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};
use tokio::sync::Mutex;

use gents_loop::loop_stream::*;
use gents_store::test_support::first_content;
use gents_toolcalls::call::hook::{DefraSessionHook, FailurePolicy};

use gents_inference::rendered_request::{AssemblyBuildPath, AssemblyTrace};
use rig::completion::{GetTokenUsage, Usage};

use gents_inference::test_support::TestDb;
use tokio::sync::Notify;

#[derive(Default)]
pub struct StreamEntryGate {
    pub entered: Notify,
    pub release: Notify,
}

pub enum ScriptedCall {
    Turn(Vec<RawStreamingChoice<()>>),
    FailStream(CompletionError),
    TurnWithMidStreamError(Vec<RawStreamingChoice<()>>, CompletionError),
}

#[derive(Clone)]
pub struct ScriptedModel {
    pub calls: Arc<Mutex<VecDeque<ScriptedCall>>>,
    seen_histories: Arc<Mutex<Vec<Vec<Message>>>>,
    seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
    seen_max_tokens: Arc<Mutex<Vec<Option<u64>>>>,
    seen_requests: Arc<Mutex<Vec<CompletionRequest>>>,
    stall_after_chunks: bool,
    capture_requests: bool,
    stream_entry_gate: Option<Arc<StreamEntryGate>>,
    provider_observer: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}

impl ScriptedModel {
    pub fn new(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self::new_turns(vec![chunks])
    }

    pub fn new_turns(turns: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self::new_calls(turns.into_iter().map(ScriptedCall::Turn).collect())
    }

    pub fn new_calls(calls: Vec<ScriptedCall>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(calls.into())),
            seen_histories: Arc::new(Mutex::new(Vec::new())),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            seen_max_tokens: Arc::new(Mutex::new(Vec::new())),
            seen_requests: Arc::new(Mutex::new(Vec::new())),
            stall_after_chunks: false,
            capture_requests: true,
            stream_entry_gate: None,
            provider_observer: None,
        }
    }

    pub fn with_provider_observer(mut self, observer: Arc<dyn Fn(bool) + Send + Sync>) -> Self {
        self.provider_observer = Some(observer);
        self
    }

    pub fn with_stream_entry_gate(mut self, gate: Arc<StreamEntryGate>) -> Self {
        self.stream_entry_gate = Some(gate);
        self
    }

    pub fn without_capture(mut self) -> Self {
        self.capture_requests = false;
        self
    }

    pub fn new_stalling(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        let mut model = Self::new_calls(vec![ScriptedCall::Turn(chunks)]);
        model.stall_after_chunks = true;
        model
    }

    pub async fn seen_histories(&self) -> Vec<Vec<Message>> {
        self.seen_histories.lock().await.clone()
    }

    pub async fn seen_tools(&self) -> Vec<Vec<String>> {
        self.seen_tools.lock().await.clone()
    }

    pub async fn seen_max_tokens(&self) -> Vec<Option<u64>> {
        self.seen_max_tokens.lock().await.clone()
    }

    pub async fn seen_requests(&self) -> Vec<CompletionRequest> {
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
        if let Some(observer) = &self.provider_observer {
            observer(true);
        }
        if let Some(gate) = &self.stream_entry_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if self.capture_requests {
            gents_inference::test_support::capture_scripted_provider_request(
                &request,
                "test-model",
            )
            .await?;
        }
        self.seen_requests.lock().await.push(request.clone());
        self.seen_histories.lock().await.push(
            request
                .chat_history
                .iter()
                .map(gents_inference::llm::rig_compat::from_rig_message)
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
        let observer = self.provider_observer.clone();
        let inner = inner.map(move |item| {
            if let Some(observer) = &observer {
                observer(false);
            }
            item
        });
        Ok(StreamingCompletionResponse::stream(Box::pin(inner)))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageResponse {
    pub usage: Option<Usage>,
}

impl GetTokenUsage for UsageResponse {
    fn token_usage(&self) -> Option<Usage> {
        self.usage
    }
}

pub fn usage_response(input_tokens: u64, output_tokens: u64, total_tokens: u64) -> UsageResponse {
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

type UsageTurns = VecDeque<Vec<RawStreamingChoice<UsageResponse>>>;
type SeenDispatch = (u64, Option<u64>);

#[derive(Clone)]
pub struct UsageScriptedModel {
    turns: Arc<Mutex<UsageTurns>>,
    seen_dispatches: Arc<Mutex<Vec<SeenDispatch>>>,
}

impl UsageScriptedModel {
    pub fn new(turns: Vec<Vec<RawStreamingChoice<UsageResponse>>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            seen_dispatches: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn seen_dispatches(&self) -> Vec<(u64, Option<u64>)> {
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
        gents_inference::test_support::capture_scripted_provider_request(&request, "test-model")
            .await?;
        self.seen_dispatches.lock().await.push((
            u64::try_from(
                completion_request_input_components(
                    &request,
                    &gents_loop::provider_input::ProviderInputCounter::new(
                        gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible,
                        gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
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

pub struct EchoTool {
    pub name: String,
    pub output: String,
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

pub struct CountingTool {
    pub name: String,
    pub output: String,
    pub calls: Arc<AtomicUsize>,
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

pub struct FixedTool {
    pub name: String,
    pub output: String,
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

pub fn echo_tool() -> Box<dyn ToolDyn> {
    Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })
}

pub fn echo_tool_turn() -> Vec<RawStreamingChoice<()>> {
    vec![
        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        )),
        RawStreamingChoice::FinalResponse(()),
    ]
}

pub fn usage_echo_tool_turn(response: UsageResponse) -> Vec<RawStreamingChoice<UsageResponse>> {
    vec![
        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        )),
        RawStreamingChoice::FinalResponse(response),
    ]
}

pub fn tool_result_text(content: &ToolResultContent) -> &str {
    match content {
        ToolResultContent::Text(text) => text.text.as_str(),
        ToolResultContent::Image(_) => "",
    }
}

pub fn config(max_turns: usize) -> LoopConfig {
    LoopConfig {
        replay: LoopReplayInput::default(),
        provider_input_counter: std::sync::Arc::new(
            gents_loop::provider_input::ProviderInputCounter::new(
                gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible,
                gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
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
        configuration_guard: None,
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: gents_inference::resolved::DEFAULT_CONTEXT_WINDOW,
        compaction_threshold: gents_inference::resolved::DEFAULT_COMPACTION_THRESHOLD,
        context_calibration: Default::default(),
        retry_policy: gents_loop::completion_retry::CompletionRetryPolicy::scheduled_default(),
        provider_idle_timeout: None,
        deadline: None,
        max_turns,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
    }
}

pub fn owned_config(max_turns: usize) -> LoopConfig {
    let mut config = config(max_turns);
    config.on_rendered_request = Some(
        gents_inference::rendered_request::scope::ambient_arming_sink(
            gents_inference::rendered_request::scope::CaptureScopeKind::Inference,
        ),
    );
    config
}

#[derive(Debug)]
pub struct AttemptEvent {
    pub turn: usize,
    pub attempt: u32,
    pub will_retry: bool,
    pub backoff: Duration,
}

#[derive(Debug, Default)]
pub struct CollectedScriptedStream {
    pub attempts: Vec<AttemptEvent>,
    pub text_chunks: Vec<String>,
    pub tool_results: Vec<String>,
    pub retractions: Vec<(usize, u32)>,
    pub final_text: Option<String>,
    pub error: Option<String>,
}

pub async fn collect_scripted_stream<S>(stream: S) -> CollectedScriptedStream
where
    S: Stream<Item = Result<LoopStreamItem, gents_loop::error::LoopStreamError>>,
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
            Ok(Some(Ok(LoopStreamItem::Text(text)))) => {
                collected.text_chunks.push(text);
            }
            Ok(Some(Ok(LoopStreamItem::ToolResult { tool_result, .. }))) => {
                collected
                    .tool_results
                    .push(tool_result_text(&tool_result.content[0]).to_string());
            }
            Ok(Some(Ok(LoopStreamItem::Final { text }))) => {
                collected.final_text = Some(text);
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

pub async fn collect_owned_scripted_stream<S>(
    stream: S,
    hook: &DefraSessionHook,
    writer: &gents_session::request::streaming::DefraStreamWriter,
    lifecycle: &mut gents_session::request::lifecycle::RequestLifecycle,
    profile: gents_loop::provider_input::ProviderInputProfile,
) -> CollectedScriptedStream
where
    S: Stream<Item = Result<LoopStreamItem, gents_loop::error::LoopStreamError>>,
{
    collect_owned_scripted_stream_with_capture_scope(stream, hook, writer, lifecycle, profile, None)
        .await
}

pub async fn collect_owned_scripted_stream_with_capture_scope<S>(
    stream: S,
    hook: &DefraSessionHook,
    writer: &gents_session::request::streaming::DefraStreamWriter,
    lifecycle: &mut gents_session::request::lifecycle::RequestLifecycle,
    profile: gents_loop::provider_input::ProviderInputProfile,
    capture_scope: Option<Arc<gents_inference::rendered_request::scope::RequestCaptureScope>>,
) -> CollectedScriptedStream
where
    S: Stream<Item = Result<LoopStreamItem, gents_loop::error::LoopStreamError>>,
{
    let scope = capture_scope.unwrap_or_else(|| {
        let context = gents_inference::rendered_request::context_for_claimed_request(
            lifecycle.request(),
            "",
            "test-model".into(),
            None,
        );
        gents_inference::rendered_request::scope::test_scope(
            context,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        )
    });
    let deadline = lifecycle.claimed_deadline_at();
    let request = lifecycle.request();
    let session_id = request.session_id.clone();
    let requester_did = request.requester_did.clone();
    let node_did = request.node_did.clone();
    let agent_did = request.agent_did.clone();
    let request_id = request.request_id.clone();
    let inherited = gents_tools::runtime_context::current_tool_runtime_context().is_some();
    let collection = gents_inference::rendered_request::scope::scope_request(scope, async move {
        futures::pin_mut!(stream);
        let doc_id = lifecycle.request().doc_id.clone();
        let mut processor = gents_loop::stream_processor::StreamProcessor::new(
            hook, writer, lifecycle, &doc_id, profile,
        );
        let mut collected = CollectedScriptedStream::default();
        while let Some(item) = stream.next().await {
            match &item {
                Ok(LoopStreamItem::AttemptFailed {
                    turn,
                    attempt,
                    will_retry,
                    backoff,
                    ..
                }) => collected.attempts.push(AttemptEvent {
                    turn: *turn,
                    attempt: *attempt,
                    will_retry: *will_retry,
                    backoff: *backoff,
                }),
                Ok(LoopStreamItem::TurnRetracted { turn, attempt, .. }) => {
                    collected.retractions.push((*turn, *attempt));
                }
                Ok(LoopStreamItem::Text(text)) => collected.text_chunks.push(text.clone()),
                Ok(LoopStreamItem::ToolResult { tool_result, .. }) => collected
                    .tool_results
                    .push(tool_result_text(&tool_result.content[0]).to_string()),
                Ok(LoopStreamItem::Final { text }) => {
                    collected.final_text = Some(text.clone());
                }
                Err(error) => collected.error = Some(error.to_string()),
                _ => {}
            }
            if let Err(error) = processor.process_item(item).await {
                collected.error = Some(error.to_string());
                break;
            }
        }
        collected
    });
    if inherited {
        collection.await
    } else {
        gents_loop::tool_call_lifecycle::runtime::scope_tool_request_identity(
            requester_did,
            Some(node_did),
            Some(agent_did),
            Some(request_id),
            gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_session(
                deadline,
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                Some(session_id),
                collection,
            ),
        )
        .await
    }
}

pub async fn owned_test_hook() -> (
    TestDb,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
) {
    owned_test_hook_with_policy(FailurePolicy::default()).await
}

pub async fn owned_test_hook_with_policy(
    policy: FailurePolicy,
) -> (
    TestDb,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
) {
    let (db, hook, writer, lifecycle, _) =
        owned_test_hook_with_identity_policy(false, policy, None, RequestSignature::Valid).await;
    (db, hook, writer, lifecycle)
}

/// Claims a request whose stored content no longer matches its signature, so
/// the claimed caller's attribution cannot be verified.
pub async fn owned_test_hook_with_unattributable_request() -> (
    TestDb,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
) {
    let (db, hook, writer, lifecycle, _) = owned_test_hook_with_identity_policy(
        false,
        FailurePolicy::default(),
        None,
        RequestSignature::AlteredAfterSigning,
    )
    .await;
    (db, hook, writer, lifecycle)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestSignature {
    Valid,
    AlteredAfterSigning,
}

/// A fixture whose requests are attributed to the node's own identity, which
/// the returned `TestDb` holds as `node_identity`.
pub async fn owned_test_hook_with_identity() -> (
    TestDb,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
) {
    let (db, hook, writer, lifecycle, _) = owned_test_hook_with_identity_policy(
        true,
        FailurePolicy::default(),
        None,
        RequestSignature::Valid,
    )
    .await;
    (db, hook, writer, lifecycle)
}

pub async fn owned_test_hook_with_folded_case(
    case: &gents_lean_vocab::families::fold_queue_contracts_turn_input::TurnInputCase,
) -> (
    TestDb,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
    rapidhash::RapidHashMap<u64, String>,
) {
    owned_test_hook_with_identity_policy(
        true,
        FailurePolicy::default(),
        Some(case),
        RequestSignature::Valid,
    )
    .await
}

/// Writes one signed request through its managed Agent, the actor that owns the
/// row, and returns its document id with the row read back through the same access.
async fn write_signed_owned_request(
    access: &gents_store::config_client::ConfigAccess,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> (String, gents_protocol::row::AgentRequestRow) {
    let (doc_id, row) = write_owned_request(access, create).await;
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
    (doc_id, row)
}

async fn write_owned_request(
    access: &gents_store::config_client::ConfigAccess,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> (String, gents_protocol::row::AgentRequestRow) {
    let result = access
        .write(
            "test.owned_loop_request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&result, "AgentRequest").unwrap();
    let loaded = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} }} }}"#,
            gents_store::graphql::escape_graphql_string(&doc_id),
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(loaded["data"]["AgentRequest"][0].clone()).unwrap();
    (doc_id, row)
}

async fn owned_test_hook_with_identity_policy(
    attributed: bool,
    policy: FailurePolicy,
    folded_case: Option<&gents_lean_vocab::families::fold_queue_contracts_turn_input::TurnInputCase>,
    signature: RequestSignature,
) -> (
    TestDb,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
    rapidhash::RapidHashMap<u64, String>,
) {
    // Every fixture runs under the node's held root, so the Agent, its requests
    // and its loop writes are all signed actors.
    let db =
        gents_inference::test_support::test_db("owned-loop", crate::lifecycle_hooks::composition())
            .await;
    let root = db.node_identity.clone();
    let node = db.node.clone();
    let node_did_owned = root.did().to_owned();
    let node_did = node_did_owned.as_str();
    let requester_did = attributed.then_some(node_did);
    let binding = gents_store::identity::provision_agent_identity(root.as_ref()).unwrap();
    let agent_identity =
        gents_store::identity::resolve_agent_identity(root.as_ref(), &binding).unwrap();
    let agent_did = binding.agent_did.clone();
    gents_inference::test_support::install_test_agent_binding(
        &node,
        node_did,
        "Owned loop test agent",
        &binding,
    )
    .await;
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        ::identity::Did::new(&agent_did).unwrap(),
        agent_identity.as_ref(),
    )
    .unwrap();
    let session_id = uuid::Uuid::new_v4().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut create = gents_session::request::lifecycle::build_signed_request(
        gents_session::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            gents_session::request::lifecycle::RequestIdentity {
                requester_did: requester_did.map(str::to_owned),
                request_id: request_id.clone(),
                node_did: node_did.to_owned(),
                agent_did: agent_did.clone(),
                session_id: session_id.clone(),
                content: "owned loop test".to_owned(),
                execution_origin: gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: now,
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                agent_did.clone(),
            ),
        ),
        gents_session::request::lifecycle::RequestSigner::Identity(agent_identity.as_ref()),
    )
    .await
    .unwrap();
    let (request_doc_id, row) = match signature {
        RequestSignature::Valid => write_signed_owned_request(&agent_access, &create).await,
        RequestSignature::AlteredAfterSigning => {
            create.content.push_str(" changed after signing");
            let written = write_owned_request(&agent_access, &create).await;
            assert!(
                gents_store::identity::request_receipt::verify_request_receipt_signature(
                    &written.1
                )
                .is_err()
            );
            written
        }
    };
    let mut folded_docs = rapidhash::RapidHashMap::default();
    if let Some(case) = folded_case {
        let mut predecessor = request_id.clone();
        for folded in &case.folded {
            let id = format!("{request_id}-folded-{}", folded.request_id);
            let mut spec = gents_session::request::lifecycle::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents_session::request::lifecycle::RequestIdentity {
                    requester_did: Some(node_did.to_owned()),
                    request_id: id.clone(),
                    node_did: node_did.to_owned(),
                    agent_did: agent_did.clone(),
                    session_id: session_id.clone(),
                    content: folded.content.clone(),
                    execution_origin:
                        gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                    agent_did.clone(),
                ),
            );
            spec.input.queue = Some(gents_protocol::request_input::RequestQueue {
                source: gents_protocol::request_input::QueueSource::User,
                policy: gents_protocol::request_input::QueuePolicy::Append,
                // Lean `Session.ordinary_queued_input_never_folds`: a folded
                // entry is a `queuedUserMessage`, which requires an explicit steer.
                delivery: gents_protocol::request_input::QueueDelivery::Steer,
                position: None,
                key: None,
                queued_after_request_id: Some(predecessor),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
            let create = gents_session::request::lifecycle::build_signed_request(
                spec,
                gents_session::request::lifecycle::RequestSigner::Identity(agent_identity.as_ref()),
            )
            .await
            .unwrap();
            let (doc, _) = write_signed_owned_request(&agent_access, &create).await;
            folded_docs.insert(folded.request_id, doc);
            predecessor = id;
        }
    }
    let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_node_did(
        node.clone(),
        row.try_into().unwrap(),
        60,
    );
    lifecycle
        .bind_managed_agent(agent_identity.as_ref())
        .unwrap();
    if let Some(case) = folded_case {
        let admitted = case
            .folded
            .iter()
            .map(|folded| folded_docs[&folded.request_id].clone())
            .collect();
        lifecycle.set_fold_admitted(admitted);
    }
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    // The claim projects the session as the admitted Agent, exactly as the
    // daemon does; the hook then resumes and writes as that Agent.
    // A local-self request's requester is its signing Agent, so the claimed
    // row, not the fixture's root, carries the session's requester scope.
    let claimed_requester = lifecycle.request().requester_did.clone();
    let hook = DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        &session_id,
        node_did,
        claimed_requester.as_deref(),
        ::identity::Did::new(&agent_did).unwrap(),
        policy,
    )
    .await
    .unwrap()
    .with_managed_identity(agent_identity.as_ref())
    .unwrap();
    // The request is agent-owned, so the node identity cannot resolve its
    // document id by logical id. Bind the id the Agent's write returned.
    hook.set_active_request_binding(
        Some(request_id),
        Some(request_doc_id),
        claimed_requester.clone(),
    )
    .await;
    // The claimed execution deadline bounds the hook, as the daemon binds it.
    hook.set_request_deadline_at(lifecycle.claimed_deadline_at())
        .await;
    // The loop's writes to the agent-owned request run as the claimed Agent.
    let writer = gents_session::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        &lifecycle,
        Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    (db, hook, writer, lifecycle, folded_docs)
}

/// The loop's rows are owned by the claimed Agent and invisible to the node
/// identity, so fixture observations read them as that Agent.
pub async fn agent_query(
    reader: &gents_store::config_client::ConfigAccess,
    query: &str,
) -> serde_json::Value {
    reader
        .execute(query)
        .await
        .expect("Agent-scoped fixture read")
}

/// [`agent_query`] in the node's response shape for fixtures that inspect it.
pub async fn agent_response(
    reader: &gents_store::config_client::ConfigAccess,
    query: &str,
) -> defra_node::QueryResponse {
    defra_node::QueryResponse::success(agent_query(reader, query).await["data"].clone())
}

/// Canonical tool output for one call, read as the claimed Agent in the
/// request's own requester scope.
pub async fn agent_tool_output(
    lifecycle: &gents_session::request::lifecycle::RequestLifecycle,
    tool_doc_id: &str,
) -> String {
    let request = lifecycle.request();
    gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
        &lifecycle.access().expect("claimed Agent access"),
        tool_doc_id,
        &request.doc_id,
        &request.session_id,
        &request.node_did,
        request.requester_did.as_deref(),
    )
    .await
    .expect("Agent-scoped canonical tool output")
    .into_text()
}

pub fn transient_provider_error(label: &str) -> CompletionError {
    CompletionError::ProviderError(format!("status code 503: {label}"))
}

pub fn permanent_provider_error() -> CompletionError {
    CompletionError::ProviderError("status code 400: duplicate field max_tokens".to_string())
}

pub fn parse_400_text(tag: &str) -> String {
    format!("BadRequestError: Expecting value [{tag}]: line 1 column 28 (char 27)")
}

pub fn parse_400_error(tag: &str) -> CompletionError {
    CompletionError::ProviderError(parse_400_text(tag))
}

pub fn assert_duration_in_range(delay: Duration, low_ms: u64, high_ms: u64) {
    let actual_ms = delay.as_millis() as u64;
    assert!(
        actual_ms >= low_ms && actual_ms <= high_ms,
        "expected duration in [{low_ms}, {high_ms}]ms, got {actual_ms}ms"
    );
}

pub fn history_has_control_char_tool_arg(history: &[Message]) -> bool {
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

pub fn history_has_tool_call(history: &[Message], tool_name: &str) -> bool {
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

pub fn history_has_tool_result_text(history: &[Message], expected: &str) -> bool {
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

pub fn json_value_has_control_char(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text.chars().any(char::is_control),
        serde_json::Value::Array(values) => values.iter().any(json_value_has_control_char),
        serde_json::Value::Object(map) => map.values().any(json_value_has_control_char),
        _ => false,
    }
}

pub mod capture;

pub mod invalid_tool_progress;

pub mod repeated_tool_failure;

pub mod request_assembly;

pub mod schema_argument_repair;

pub mod streaming;

pub mod tool_execution;
