use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use gents_inference::llm::message::{AssistantContent, ToolResultContent, UserContent};
use gents_loop::test_support::scripted_provider::{Reply, ScriptedProvider};
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use rig::completion::CompletionError;

use gents_inference::llm::message::Message;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall};

use gents_loop::loop_stream::*;
use gents_store::test_support::first_content;
use gents_toolcalls::call::hook::{DefraSessionHook, FailurePolicy};

use gents_inference::rendered_request::{AssemblyBuildPath, AssemblyTrace};

use gents_store::test_support::TestStore;

/// The scripted provider as the owned-loop suites drive it: each call
/// answers with the next of `replies`, then with a bare final response, and
/// sends its OpenAI body through the capturing transport.
pub fn scripted<R>(replies: impl IntoIterator<Item = R>) -> ScriptedProvider
where
    R: Into<Reply>,
{
    ScriptedProvider::new(replies.into_iter().map(Into::into))
        .then_end()
        .capturing_openai("test-model")
}

pub struct EchoTool {
    pub name: String,
    pub output: String,
}

impl ToolDyn for EchoTool {
    fn hidden(&self) -> bool {
        false
    }
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

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a gents_loop::tool::ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.output.clone()) })
    }
}

pub struct CountingTool {
    pub name: String,
    pub output: String,
    pub calls: Arc<AtomicUsize>,
}

impl ToolDyn for CountingTool {
    fn hidden(&self) -> bool {
        false
    }
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

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a gents_loop::tool::ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
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
    fn hidden(&self) -> bool {
        false
    }
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

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a gents_loop::tool::ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
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

pub fn tool_result_text(content: &ToolResultContent) -> &str {
    match content {
        ToolResultContent::Text(text) => text.text.as_str(),
        ToolResultContent::Image(_) => "",
    }
}

pub fn config(max_turns: usize) -> LoopConfig {
    LoopConfig {
        provider_call: Default::default(),
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
        tool_rebinding: None,
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
        max_invalid_tool_calls: gents_loop::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: gents_loop::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
        tool_context: gents_loop::tool::ToolCallEnvelope::default(),
    }
}

/// The envelope the tools of the claimed request `lifecycle` are called
/// with: its identity, session and claimed deadline.
pub fn request_call(
    lifecycle: &gents_session::request::lifecycle::RequestLifecycle,
) -> gents_loop::tool::ToolCallEnvelope {
    let request = lifecycle.request();
    gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry {
        identity: Some(
            gents_loop::tool_call_lifecycle::runtime::ToolRequestIdentity::new(
                request.requester_did.clone(),
                Some(request.node_did.clone()),
                Some(request.agent_did.clone()),
                Some(request.request_id.clone()),
            ),
        ),
        session_id: Some(request.session_id.clone()),
        ..gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry::new(
            lifecycle.claimed_deadline_at(),
            tokio_util::sync::CancellationToken::new(),
        )
    }
    .call()
}

/// [`owned_config`] for the claimed request `lifecycle`.
pub fn owned_config_for(
    max_turns: usize,
    lifecycle: &gents_session::request::lifecycle::RequestLifecycle,
) -> LoopConfig {
    let mut config = owned_config(max_turns);
    config.tool_context = request_call(lifecycle);
    config
}

/// A config whose calls are captured, into a sink that keeps nothing.
pub fn owned_config(max_turns: usize) -> LoopConfig {
    let mut config = config(max_turns);
    config.on_rendered_request = Some(gents_inference::rendered_request::scope::arming_sink(
        gents_inference::rendered_request::scope::CaptureScopeKind::Inference,
    ));
    config.provider_call.capture = Some(gents_inference::rendered_request::scope::test_scope(
        gents_inference::rendered_request::RenderedRequestContext {
            request_doc_id: "owned-test-doc".into(),
            request_commit_cid: String::new(),
            request_id: "owned-test-request".into(),
            node_did: "did:test:node".into(),
            requester_did: String::new(),
            agent_did: "general".into(),
            session_id: "owned-test-session".into(),
            model_name: "test-model".into(),
            provider_family: None,
        },
        Arc::new(|_| Box::pin(async { Ok(()) })),
    ));
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
    let collection = async move {
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
    };
    collection.await
}

pub async fn owned_test_hook() -> (
    TestStore,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
) {
    owned_test_hook_with_policy(FailurePolicy::default()).await
}

pub async fn owned_test_hook_with_policy(
    policy: FailurePolicy,
) -> (
    TestStore,
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
    TestStore,
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
/// the returned `TestStore` holds as `node_identity`.
pub async fn owned_test_hook_with_identity() -> (
    TestStore,
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
    TestStore,
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
    folded_case: Option<
        &gents_lean_vocab::families::fold_queue_contracts_turn_input::TurnInputCase,
    >,
    signature: RequestSignature,
) -> (
    TestStore,
    DefraSessionHook,
    gents_session::request::streaming::DefraStreamWriter,
    gents_session::request::lifecycle::RequestLifecycle,
    rapidhash::RapidHashMap<u64, String>,
) {
    // Every fixture runs under the node's held root, so the Agent, its requests
    // and its loop writes are all signed actors.
    let db = gents_store::test_support::TestStore::start(
        "owned-loop",
        crate::lifecycle_hooks::composition(),
    )
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
        // The claim's deadline bounds every tool call of the owned loop; no
        // test here exercises that cut (`WallClockBound` `"unreached"`).
        gents_store::test_support::UNREACHED_DEADLINE.as_secs(),
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
