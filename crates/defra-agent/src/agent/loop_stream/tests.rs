use std::collections::VecDeque;
use std::sync::Arc;

use crate::llm::message::{AssistantContent, ToolResultContent, UserContent};
use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use futures::{stream, StreamExt};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};

use crate::llm::message::Message;
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent, StreamedUserContent,
    StreamingCompletionResponse,
};
use tokio::sync::Mutex;

use super::*;
use crate::ensure_schemas;
use crate::hook::{DefraSessionHook, FailurePolicy};
use crate::test_support::first_content;

/// A `CompletionModel` whose `stream` replays one scripted turn per call: each
/// `stream()` pops the next `Vec<RawStreamingChoice>` from the queue, letting a
/// test drive a multi-turn (tool-call then text) loop without a provider. Once
/// the queue is empty it yields a bare final response so the loop terminates.
#[derive(Clone)]
struct ScriptedModel {
    turns: Arc<Mutex<VecDeque<Vec<RawStreamingChoice<()>>>>>,
    /// `chat_history` of every request the loop sent, in order (converted to
    /// native at the capture boundary) — lets a test assert how the loop
    /// threaded prior turns back to the provider.
    seen_histories: Arc<Mutex<Vec<Vec<Message>>>>,
    /// Advertised tool names (`request.tools`) of every request the loop sent,
    /// in order — lets a test assert the toolset is attached on every turn.
    seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
    /// When set, every turn's stream yields its scripted chunks then hangs
    /// (never reaches EOF), simulating a provider that stalls mid-turn.
    stall_after_chunks: bool,
}

impl ScriptedModel {
    fn new(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self::new_turns(vec![chunks])
    }

    fn new_turns(turns: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            seen_histories: Arc::new(Mutex::new(Vec::new())),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            stall_after_chunks: false,
        }
    }

    /// A single turn that emits `chunks` then stalls forever instead of ending.
    fn new_stalling(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        let mut model = Self::new_turns(vec![chunks]);
        model.stall_after_chunks = true;
        model
    }

    async fn seen_histories(&self) -> Vec<Vec<Message>> {
        self.seen_histories.lock().await.clone()
    }

    async fn seen_tools(&self) -> Vec<Vec<String>> {
        self.seen_tools.lock().await.clone()
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
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        self.seen_histories.lock().await.push(
            _request
                .chat_history
                .iter()
                .map(crate::llm::rig_compat::from_rig_message)
                .collect(),
        );
        self.seen_tools.lock().await.push(
            _request
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
        );
        let chunks = self
            .turns
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| vec![RawStreamingChoice::FinalResponse(())]);
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
            chunks.into_iter().map(Ok).collect();
        let inner: rig::streaming::StreamingResult<()> = if self.stall_after_chunks {
            Box::pin(stream::iter(items).chain(stream::pending()))
        } else {
            Box::pin(stream::iter(items))
        };
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

/// A trivial tool that echoes a fixed output, for dispatch tests.
struct EchoTool {
    name: String,
    output: String,
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

/// A tool whose call always returns a fixed string (used for large/managed
/// outputs); name defaults to "echo".
struct FixedTool {
    name: String,
    output: String,
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
struct RecordingDefinitionTool {
    seen_prompt: Arc<Mutex<Option<String>>>,
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

fn echo_tool() -> Box<dyn ToolDyn> {
    Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })
}

/// Script one tool-calling turn that invokes `echo`.
fn echo_tool_turn() -> Vec<RawStreamingChoice<()>> {
    vec![
        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        )),
        RawStreamingChoice::FinalResponse(()),
    ]
}

/// Set the request context that on_tool_call/on_tool_result require. The
/// session itself is created by the generator's per-turn on_completion_call.
async fn ready_hook_for(hook: &DefraSessionHook) {
    hook.set_active_request_id(Some(uuid::Uuid::new_v4().to_string()))
        .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
}

fn tool_result_text(content: &ToolResultContent) -> &str {
    match content {
        ToolResultContent::Text(text) => text.text.as_str(),
        ToolResultContent::Image(_) => "",
    }
}

fn config(max_turns: usize) -> LoopConfig {
    LoopConfig {
        preamble: None,
        context_message: None,
        temperature: None,
        max_tokens: None,
        additional_params: None,
        tool_choice: None,
        on_rendered_request: None,
        retry_policy: crate::agent::completion_retry::CompletionRetryPolicy::scheduled_default(),
        deadline: None,
        max_turns,
    }
}

async fn test_hook() -> (Arc<defra_node::EmbeddedNode>, DefraSessionHook) {
    let data_path =
        std::env::temp_dir().join(format!("agent-loop-stream-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();
    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:test",
        FailurePolicy::default(),
    );
    (node, hook)
}

#[tokio::test]
async fn single_turn_no_tools_yields_text_then_final() {
    let (_node, hook) = test_hook().await;
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("Hello ".to_string()),
        RawStreamingChoice::Message("world".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);

    let stream = run_loop_stream(
        model,
        Some(hook),
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    futures::pin_mut!(stream);

    let mut texts = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("loop item should be Ok") {
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text)) => {
                texts.push(text.text);
            }
            MultiTurnStreamItem::FinalResponse(final_response) => {
                final_text = Some(final_response.response().to_string());
            }
            _ => {}
        }
    }

    assert_eq!(texts, vec!["Hello ".to_string(), "world".to_string()]);
    assert_eq!(final_text.as_deref(), Some("Hello world"));
}

#[tokio::test]
async fn rendered_request_sink_runs_before_provider_stream() {
    let (_node, hook) = test_hook().await;
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("unreached".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let captures = Arc::new(Mutex::new(Vec::new()));
    let captures_for_sink = captures.clone();
    let mut loop_config = config(0);
    loop_config.on_rendered_request = Some(Arc::new(move |turn_index, request| {
        let captures = captures_for_sink.clone();
        Box::pin(async move {
            captures
                .lock()
                .await
                .push((turn_index, request.chat_history.len()));
            Err(anyhow::anyhow!("capture failed"))
        })
    }));

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    futures::pin_mut!(stream);

    let item = stream
        .next()
        .await
        .expect("stream should yield the sink error");
    let error = item.expect_err("capture failure should abort the provider call");
    assert!(
        format!("{error:?}").contains("capturing rendered completion request failed"),
        "unexpected error: {error:?}"
    );
    assert_eq!(captures.lock().await.as_slice(), &[(0, 1)]);
    assert!(
        model.seen_histories().await.is_empty(),
        "provider stream must not start after capture failure"
    );
}

#[tokio::test]
async fn context_message_is_sent_before_prompt() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("ok".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let mut cfg = config(0);
    cfg.context_message = Some(Message::user(
        "<context>\nnow=2026-06-15T00:00:00Z\n</context>",
    ));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("actual prompt"),
        Vec::new(),
        Arc::new(Vec::new()),
        cfg,
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 1);
    assert!(matches!(
        &histories[0][0],
        Message::User { content }
            if matches!(first_content(&content), UserContent::Text(text) if text.text.starts_with("<context>"))
    ));
}

#[tokio::test]
async fn tool_call_turn_executes_threads_result_and_completes() {
    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;
    let prompt = Message::user("use the echo tool");

    // Turn 1: the model calls `echo`. Turn 2: it answers with text.
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })];

    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    let mut tool_results = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("loop item should be Ok") {
            MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            }) => {
                tool_results.push(
                    tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                        &tool_result.content.first(),
                    ))
                    .to_string(),
                );
            }
            MultiTurnStreamItem::FinalResponse(final_response) => {
                final_text = Some(final_response.response().to_string());
            }
            _ => {}
        }
    }

    // The tool ran, its (bounded) result was threaded/yielded, and the loop
    // reached a text response on the next turn.
    assert_eq!(tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(final_text.as_deref(), Some("done"));

    // The generator drove the tool-call lifecycle directly: on_tool_call started
    // it and on_tool_result completed it with the result. (The tool-result
    // *message* persistence is split with StreamProcessor — exercised once the
    // generator is wired into the consumer in step 3 — so it is not asserted
    // here against the standalone generator.)
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state result } }")
        .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|value| value.as_str()) == Some("echo")
                && row.get("lifecycle_state").and_then(|value| value.as_str()) == Some("completed")
                && row
                    .get("result")
                    .and_then(|value| value.as_str())
                    .is_some_and(|result| result.contains("ECHOED"))
        }),
        "expected a completed echo tool call recording the result; rows: {rows:?}"
    );
}

#[tokio::test]
async fn tool_executes_before_provider_stalls_mid_stream() {
    // P2 regression: a provider that emits a tool call then stalls before EOF
    // must still have its tool executed. Rig runs each tool inline as its
    // ToolCall arrives, so the lifecycle / AgentToolCall row exists before the
    // stall; the daemon liveness timeout then has something to cancel. The old
    // design collected tool calls and dispatched only after the stream drained,
    // so a mid-stream stall left the tool unrun with nothing to mark.
    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;
    let prompt = Message::user("use the echo tool then stall");

    // One turn: emit a tool call, then hang (no FinalResponse, no EOF).
    let model = ScriptedModel::new_stalling(vec![RawStreamingChoice::ToolCall(
        RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        ),
    )]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })];

    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    // Item 1 is the tool call. Resuming the stream then runs the tool inline and
    // afterwards blocks forever on the stalled provider — so the second poll
    // never returns, but the tool executes before that block. Bound it.
    let first = stream.next().await.expect("should yield the tool call");
    assert!(
        matches!(
            first,
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ToolCall { .. }
            ))
        ),
        "first item should be the tool call; got {first:?}"
    );
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(3), stream.next()).await;

    // Despite the stall, the tool ran to completion (its row exists, recorded).
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state result } }")
        .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|value| value.as_str()) == Some("echo")
                && row
                    .get("result")
                    .and_then(|value| value.as_str())
                    .is_some_and(|result| result.contains("ECHOED"))
        }),
        "tool must execute inline before the provider stall; rows: {rows:?}"
    );
}

#[tokio::test]
async fn tool_definition_receives_prompt_rag_text() {
    // P3/compat: tool definitions must be built with the prompt's rag text (rig
    // parity), not String::new(), so prompt-aware tools keep the task context.
    let (_node, hook) = test_hook().await;
    ready_hook_for(&hook).await;
    let seen = Arc::new(Mutex::new(None));
    let tool: Box<dyn ToolDyn> = Box::new(RecordingDefinitionTool {
        seen_prompt: seen.clone(),
    });

    // A single text-only turn; the tool is never called, but its definition is
    // still requested when the request is built.
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("hi".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        Message::user("teach me rust"),
        Vec::new(),
        Arc::new(vec![tool]),
        config(1),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    assert_eq!(
        seen.lock().await.as_deref(),
        Some("teach me rust"),
        "tool definition should receive the prompt's rag text, not an empty string"
    );
}

#[tokio::test]
async fn exceeding_max_turns_terminates_with_error() {
    let (_node, hook) = test_hook().await;
    let prompt = Message::user("loop");
    ready_hook_for(&hook).await;

    // max_turns = 0 permits one tool round-trip (2 completions, matching rig);
    // a model that keeps calling tools is blocked on the completion past the cap
    // and surfaces a max-turns error.
    let model = ScriptedModel::new_turns(vec![echo_tool_turn(), echo_tool_turn()]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(0),
    );
    futures::pin_mut!(stream);

    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item);
    }

    let last = items.last().expect("stream should yield at least one item");
    assert!(last.is_err(), "expected a terminal error; got {last:?}");
    // Permanent `StreamingError::Prompt(MaxTurnsError)` (rig's variant), not a
    // retryable `Completion(ResponseError)` — turn exhaustion must not retry.
    let error = last.as_ref().err().unwrap();
    assert!(
        matches!(
            error,
            rig::agent::StreamingError::Prompt(prompt_error)
                if matches!(**prompt_error, rig::completion::PromptError::MaxTurnsError { .. })
        ),
        "expected a max-turns Prompt error; got {last:?}"
    );
    // And it must classify as a permanent failure: retrying turn exhaustion would
    // re-run the loop (and its tools) to no purpose.
    assert!(
        !crate::error::classify_completion_error(error).is_retryable(),
        "max-turns exhaustion must be non-retryable; got {last:?}"
    );
}

#[tokio::test]
async fn managed_terminal_tool_result_terminates_loop() {
    let (_node, hook) = test_hook().await;
    let prompt = Message::user("run the slow tool");
    ready_hook_for(&hook).await;

    // The tool returns a managed timeout marker; on_tool_result classifies it as
    // a terminal timeout and the loop ends with an error rather than continuing.
    let marker = crate::tool_call_lifecycle::runtime::timeout_result(None);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".to_string(),
        output: marker,
    })];
    let model = ScriptedModel::new_turns(vec![echo_tool_turn()]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item);
    }

    let last = items.last().expect("stream should yield at least one item");
    assert!(last.is_err(), "expected a terminal error; got {last:?}");
    assert!(
        format!("{:?}", last.as_ref().err().unwrap()).contains("deadline"),
        "expected a deadline/timeout terminate; got {last:?}"
    );
}

#[tokio::test]
async fn threaded_assistant_turn_carries_provider_message_id() {
    // P2a regression: the in-loop assistant message threaded back to the provider
    // must carry the provider message id (OpenAI Responses / ChatGPT Codex
    // follow-up requests reference prior `msg_` ids). Turn 1 emits a MessageId
    // plus a tool call; the tool result drives turn 2, whose request history must
    // contain the assistant tool-call message tagged with that id.
    let (_node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::MessageId("msg_abc123".to_string()),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("go"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        2,
        "expected two completion turns; got {histories:?}"
    );
    let assistant_id = histories[1].iter().find_map(|message| match message {
        Message::Assistant { id, .. } => Some(id.clone()),
        _ => None,
    });
    assert_eq!(
        assistant_id,
        Some(Some("msg_abc123".to_string())),
        "threaded assistant turn must carry the provider message id; history: {:?}",
        histories[1]
    );
}

#[tokio::test]
async fn toolset_is_attached_to_every_completion_request_in_the_loop() {
    // Regression for the CLI tool-loop test: rig's Agent re-sent the full tool
    // list on every turn; the owned loop must too. The follow-up request after a
    // tool result is folded in (turn 2) must still advertise the toolset, or the
    // provider sees a tool-result conversation with no tools.
    let (_node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    // Turn 1: the model calls `echo`. Turn 2: it answers with text.
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let seen_tools = model.seen_tools().await;
    assert_eq!(
        seen_tools.len(),
        2,
        "expected two completion turns; got {seen_tools:?}"
    );
    for (turn, tools) in seen_tools.iter().enumerate() {
        assert!(
            tools.contains(&"echo".to_string()),
            "completion request for turn {} must advertise the toolset; got {seen_tools:?}",
            turn + 1
        );
    }
}

/// Provider-request invariants: what every completion request the loop emits
/// must satisfy, independent of the scenario that produced it. Mirrors the
/// provider-side contract that `sanitize_history_for_provider` enforces for
/// loaded history — the loop must satisfy it by construction for the messages
/// it threads itself.
fn assert_provider_request_invariants(turn: usize, history: &[Message]) {
    let mut pending_call_keys: Vec<String> = Vec::new();
    for message in history {
        match message {
            Message::Assistant { content, .. } => {
                assert!(
                    pending_call_keys.is_empty(),
                    "turn {turn}: assistant message before prior turn's tool calls were resolved"
                );
                // Ordering: no text or reasoning after a tool call.
                let mut seen_tool_call = false;
                for item in content.iter() {
                    match item {
                        AssistantContent::ToolCall(tool_call) => {
                            seen_tool_call = true;
                            pending_call_keys.push(
                                tool_call
                                    .call_id
                                    .clone()
                                    .unwrap_or_else(|| tool_call.id.clone()),
                            );
                        }
                        _ => assert!(
                            !seen_tool_call,
                            "turn {turn}: assistant content after a tool call (providers reject)"
                        ),
                    }
                }
            }
            Message::User { content } => {
                let has_tool_results = content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_)));
                assert!(
                    has_tool_results || pending_call_keys.is_empty(),
                    "turn {turn}: ordinary user content before prior tool calls were resolved"
                );
                for item in content.iter() {
                    if let UserContent::ToolResult(tool_result) = item {
                        let key = tool_result
                            .call_id
                            .clone()
                            .unwrap_or_else(|| tool_result.id.clone());
                        let position = pending_call_keys.iter().position(|call| call == &key);
                        assert!(
                            position.is_some(),
                            "turn {turn}: tool result '{key}' without a preceding tool call"
                        );
                        pending_call_keys.remove(position.unwrap());
                    }
                }
            }
            Message::System { .. } => assert!(
                pending_call_keys.is_empty(),
                "turn {turn}: system message before prior tool calls were resolved"
            ),
        }
    }
    assert!(
        pending_call_keys.is_empty(),
        "turn {turn}: unpaired tool calls reached the provider: {pending_call_keys:?}"
    );
}

#[tokio::test]
async fn every_request_in_a_tool_loop_satisfies_provider_invariants() {
    // Conformance guard for the loop's own threading: across a multi-tool,
    // multi-turn run, every request's history must pair calls with results and
    // keep assistant content provider-ordered — by construction, no sanitizer.
    let (_node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    let model = ScriptedModel::new_turns(vec![
        // Turn 1: text + reasoning + two tool calls in one assistant turn.
        vec![
            RawStreamingChoice::Message("let me check".to_string()),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-2".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        // Turn 2: one more tool call.
        echo_tool_turn(),
        // Turn 3: final text.
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("run the tools"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        3,
        "expected three completion turns; got {histories:?}"
    );
    for (index, history) in histories.iter().enumerate() {
        assert_provider_request_invariants(index + 1, history);
    }
}

#[tokio::test]
async fn dirty_caller_history_is_sanitized_at_loop_entry() {
    // Chokepoint guarantee: EVERY owned-loop consumer (daemon, oneshot,
    // compaction summarize, title, subagent children) sends provider-valid
    // history because the loop sanitizes the caller-provided history at entry
    // — no call site can forget the sanitizer. Feed a dirty history (unpaired
    // call, orphaned result, text-after-call ordering) and assert the request
    // on the wire satisfies the provider invariants.
    let (_node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    let unpaired_call = crate::llm::message::ToolCall {
        id: "call-unpaired".to_string(),
        call_id: Some("call-unpaired".to_string()),
        function: crate::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    };
    let paired_call = crate::llm::message::ToolCall {
        id: "call-paired".to_string(),
        call_id: Some("call-paired".to_string()),
        function: crate::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    };
    let dirty_history = vec![
        // Orphaned result: its call was compacted away.
        Message::User {
            content: vec![UserContent::tool_result(
                "call-gone".to_string(),
                vec![crate::llm::message::ToolResultContent::text("orphaned")],
            )],
        },
        // Misordered assistant turn (text AFTER calls) with one unpaired call.
        Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::ToolCall(paired_call),
                AssistantContent::ToolCall(unpaired_call),
                AssistantContent::Text(crate::llm::message::Text {
                    text: "stale ordering".to_string(),
                }),
            ],
        },
        Message::User {
            content: vec![UserContent::tool_result(
                "call-paired".to_string(),
                vec![crate::llm::message::ToolResultContent::text("ok")],
            )],
        },
    ];

    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("hi".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("continue"),
        dirty_history,
        Arc::new(Vec::new()),
        config(1),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 1);
    assert_provider_request_invariants(1, &histories[0]);
    // The unpaired call is gone but the paired exchange survives.
    let kept_calls: Vec<String> = histories[0]
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(call) => Some(call.id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        kept_calls,
        vec!["call-paired".to_string()],
        "unpaired call dropped, paired call kept; history: {:?}",
        histories[0]
    );
}

#[tokio::test]
async fn oversized_tool_result_is_bounded_before_threading() {
    let (_node, hook) = test_hook().await;
    let prompt = Message::user("read the big thing");
    ready_hook_for(&hook).await;

    // A tool returning far more than the default limits: the model-facing
    // (threaded/yielded) result must be bounded, while on_tool_result still
    // receives the full output for spill (#401 closed natively).
    let big_line = "x".repeat(200);
    let big_output = std::iter::repeat(big_line)
        .take(10_000)
        .collect::<Vec<_>>()
        .join("\n");
    let full_len = big_output.len();
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".to_string(),
        output: big_output,
    })];
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    let mut bounded_len = None;
    while let Some(item) = stream.next().await {
        if let MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
            tool_result,
            ..
        }) = item.expect("loop item should be Ok")
        {
            bounded_len = Some(
                tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                    &tool_result.content.first(),
                ))
                .len(),
            );
        }
    }

    let bounded_len = bounded_len.expect("a tool result should have been yielded");
    assert!(
        bounded_len < full_len,
        "expected the threaded result to be bounded: bounded={bounded_len} full={full_len}"
    );
    assert!(bounded_len > 0, "bounded result should be non-empty");
}

#[tokio::test]
async fn run_loop_to_text_persists_assistant_reply() {
    // Regression: one-shot (run_loop_to_text) must persist the assistant reply,
    // not just the user prompt.
    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("the answer".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);

    let reply = run_loop_to_text(
        model,
        Some(hook.clone()),
        Message::user("the question"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    )
    .await
    .expect("run_loop_to_text should succeed");
    assert_eq!(reply, "the answer");

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert!(
        history.iter().any(|message| matches!(message,
            Message::Assistant { content, .. }
                if content.iter().any(|c| matches!(c, AssistantContent::Text(text)
                    if text.text == "the answer")))),
        "one-shot must persist the assistant reply; history: {history:?}"
    );
}

#[tokio::test]
async fn run_loop_to_text_persists_tool_using_transcript() {
    // Regression: for tool-using one-shots, both the assistant tool-call turn and
    // the tool-result message must be persisted (tool-result persistence gates on
    // the assistant turn being persisted first).
    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let reply = run_loop_to_text(
        model,
        Some(hook.clone()),
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    )
    .await
    .expect("run_loop_to_text should succeed");
    assert_eq!(reply, "done");

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert!(
        history.iter().any(|message| matches!(message,
            Message::User { content }
                if content.iter().any(|c| matches!(c, UserContent::ToolResult(result)
                    if tool_result_text(first_content(&result.content)) == "ECHOED")))),
        "tool-using one-shot must persist the tool-result message; history: {history:?}"
    );
    assert!(
        history.iter().any(|message| matches!(message,
            Message::Assistant { content, .. }
                if content.iter().any(|c| matches!(c, AssistantContent::Text(text)
                    if text.text == "done")))),
        "tool-using one-shot must persist the final assistant reply; history: {history:?}"
    );
}

#[test]
fn value_to_json_string_passes_strings_through_unquoted() {
    assert_eq!(
        value_to_json_string(&serde_json::json!("plain")),
        "plain".to_string()
    );
    assert_eq!(
        value_to_json_string(&serde_json::json!({"path": "x"})),
        r#"{"path":"x"}"#.to_string()
    );
}

#[test]
fn deadline_remaining_is_zero_when_past() {
    let past = chrono::Utc::now() - chrono::Duration::seconds(5);
    assert_eq!(
        super::deadline_remaining(Some(past)),
        Some(std::time::Duration::ZERO)
    );
    assert_eq!(super::deadline_remaining(None), None);
}

#[test]
fn assembles_context_immediately_before_prompt() {
    // Fences Lean `PromptAssembly.Template.assembleWithContext_tail`: when a
    // per-request context message is present, the assembly ends with exactly
    // [contextPreamble, prompt] — context immediately precedes the prompt.
    let context = Message::user("<context>\nseat: x\n</context>");
    let prompt = Message::user("hello");

    let with_context = super::assemble_new_messages(Some(context.clone()), prompt.clone());
    assert_eq!(with_context.len(), 2);
    assert!(super::is_request_context_message(&with_context[0]));
    assert_eq!(with_context[1], prompt);
    // Context is the immediately-preceding entry before the prompt.
    assert_eq!(&with_context[with_context.len() - 2], &context);

    // Without a context message, the prompt is the sole (last) entry.
    let without = super::assemble_new_messages(None, prompt.clone());
    assert_eq!(without, vec![prompt]);
}

#[test]
fn is_request_context_message_only_matches_context_user_text() {
    assert!(super::is_request_context_message(&Message::user(
        "<context>\nx\n</context>"
    )));
    assert!(!super::is_request_context_message(&Message::user(
        "an ordinary prompt"
    )));
    assert!(!super::is_request_context_message(&Message::assistant(
        "hi"
    )));
}

#[tokio::test]
async fn dispatch_tool_calls_known_tool_and_reports_unknown() {
    // No tool runtime scope is active in this unit test, so dispatch_tool takes
    // the unscoped path: look up by name and call directly.
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })];

    assert_eq!(
        super::dispatch_tool(&tools, "echo", "{}".to_string()).await,
        "ECHOED".to_string()
    );
    assert_eq!(
        super::dispatch_tool(&tools, "missing", "{}".to_string()).await,
        "error: unknown tool 'missing'".to_string()
    );
}

#[tokio::test]
async fn dispatch_tool_marks_unparseable_args_with_collision_free_marker() {
    use crate::llm::tool::{Tool, ToolDefinition};
    use crate::tool_call_lifecycle::runtime::unparseable_args_notice;

    // A tool whose Args require fields the (valid-JSON) call omits, so the real
    // parse seam raises UnparseableArgs.
    struct StrictArgsTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        body: String,
        #[allow(dead_code)]
        findings: Vec<String>,
    }
    impl Tool for StrictArgsTool {
        const NAME: &'static str = "strict";
        type Error = StrictToolError;
        type Args = StrictArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
            Ok("ran".to_string())
        }
    }

    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictArgsTool)];
    // Truncated mid-string: escape-only repair cannot complete it, so it stays
    // UnparseableArgs and dispatch wraps a notice in the collision-free marker
    // (which on_tool_result maps to failed(ArgumentInvalid)) — not the tool output.
    let result = super::dispatch_tool(&tools, "strict", r#"{"body":"cut off"#.to_string()).await;
    // The result must NOT use a forgeable human-readable prefix a real tool could emit.
    assert!(
        !result.starts_with("JsonError:"),
        "must not key on a collidable prefix, got: {result}"
    );
    let notice =
        unparseable_args_notice(&result).expect("dispatch must wrap the notice in the marker");
    assert!(
        !notice.contains("ran") && notice.contains("token limit"),
        "the notice must replace the tool output and guide the model to shorten, got: {notice}"
    );
}

/// Loop-level fence: an unparseable-args tool call (a) does NOT run the tool,
/// (b) surfaces a clean notice to the model (the internal marker stripped) so it
/// can re-emit corrected arguments next turn, and (c) terminalizes the started
/// `AgentToolCall` as `failed`/`argumentInvalid` via `on_tool_result`. This
/// preserves the tool-call liveness invariant (Lean
/// `ToolExecution.live_call_reaches_terminal`, T5: the started call reaches a
/// terminal state) using the proven `Running → Failed` edge with the existing
/// `FailureClass::ArgumentInvalid`.
#[tokio::test]
async fn unparseable_tool_args_notify_model_and_terminalize_failed() {
    use crate::llm::tool::{Tool, ToolDefinition};

    struct StrictArgsTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        report_type: String,
        #[allow(dead_code)]
        findings: Vec<String>,
    }
    impl Tool for StrictArgsTool {
        const NAME: &'static str = "post_status";
        type Error = StrictToolError;
        type Args = StrictArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
            // Must NOT run: the args never deserialize.
            panic!("the tool must not run on unparseable arguments");
        }
    }

    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    // Valid JSON, but missing the required `findings` field: a Malformed parse
    // failure that no repair can recover into the typed args.
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "post_status".to_string(),
                serde_json::json!({ "report_type": "steward" }),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictArgsTool)];

    let stream = run_loop_stream(
        model,
        Some(hook),
        Message::user("post a status report"),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    // The model is notified via a tool result (no error ends the stream); it sees
    // the clean notice and answers on the next turn.
    let mut tool_results = Vec::new();
    while let Some(item) = stream.next().await {
        if let MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
            tool_result,
            ..
        }) = item.expect("loop must not fail; unparseable args are notified, not raised")
        {
            tool_results.push(
                tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                    &tool_result.content.first(),
                ))
                .to_string(),
            );
        }
    }
    assert!(
        tool_results
            .iter()
            .any(|r| r.contains("could not be parsed")),
        "the model must be notified with a clean parse-failure notice, got: {tool_results:?}"
    );
    assert!(
        !tool_results
            .iter()
            .any(|r| r.contains("__defra_agent_tool_lifecycle__")),
        "the internal marker must never leak to the model, got: {tool_results:?}"
    );

    // T5: the started call terminalized failed(argumentInvalid) — via on_tool_result
    // stripping the marker and forcing ArgumentInvalid — instead of dangling in `running`.
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state tool_failure_class } }")
        .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|v| v.as_str()) == Some("post_status")
                && row.get("lifecycle_state").and_then(|v| v.as_str()) == Some("failed")
                && row.get("tool_failure_class").and_then(|v| v.as_str()) == Some("argumentInvalid")
        }),
        "the started tool call must terminalize failed/argumentInvalid, got rows: {rows:?}"
    );
}

/// #589/#590 incident regression, salvageable half: the model emits the exact
/// production corrupt-arguments payload (leaked `</think`, stray CJK, nested
/// Hermes fragment, literal newlines, duplicated keys). The escape-only repair
/// salvages the intended object, so (a) the typed tool RUNS with the intended
/// `tool_name`, (b) the durable `AgentMessage` history carries object-shaped
/// arguments — never the raw corrupt string that jammed Amy's session — and
/// (c) the next provider request sees object-shaped arguments.
#[tokio::test]
async fn corrupt_589_tool_args_salvage_runs_and_history_stays_object_shaped() {
    use crate::llm::tool::{Tool, ToolDefinition};

    struct DescribeTool;
    #[derive(Debug, thiserror::Error)]
    #[error("describe tool error")]
    struct DescribeToolError;
    #[derive(serde::Deserialize)]
    struct DescribeArgs {
        tool_name: String,
    }
    impl Tool for DescribeTool {
        const NAME: &'static str = "describe_tool";
        type Error = DescribeToolError;
        type Args = DescribeArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
            Ok(format!("described:{}", args.tool_name))
        }
    }

    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    // The wire parser could not parse the corrupt bytes, so rig carries them as
    // a raw Value::String — exactly the shape persisted in the production store.
    let poison = serde_json::Value::String(crate::test_support::CORRUPT_TOOL_ARGS_589.to_string());
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "describe_tool".to_string(),
                poison,
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(DescribeTool)];

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("describe list_hosts"),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    let mut tool_results = Vec::new();
    while let Some(item) = stream.next().await {
        if let MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
            tool_result,
            ..
        }) = item.expect("loop item should be Ok")
        {
            tool_results.push(
                tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                    &tool_result.content.first(),
                ))
                .to_string(),
            );
        }
    }

    // (a) The intended call ran: salvage recovered `tool_name: list_hosts`.
    assert_eq!(
        tool_results,
        vec!["described:list_hosts".to_string()],
        "the salvageable #589 payload must run the intended call, not waste a turn"
    );

    // (b) The next provider request carries object-shaped arguments. (The
    // durable AgentMessage fence lives in the StreamProcessor harness —
    // `stream_processor::tests::corrupt_tool_call_arguments_persist_object_shaped`
    // — since the bare generator does not persist assistant turns.)
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_all_history_tool_args_object_shaped(&histories[1]);
    drop(node);
}

/// #590 incident regression, non-salvageable half: arguments that are valid
/// JSON but not an object (the `"[]"` reproduction). The call must fail
/// `argumentInvalid` with the model notified (never run), and neither the
/// durable history nor the next provider request may carry the non-object.
#[tokio::test]
async fn nonobject_tool_args_never_reach_durable_history_or_provider() {
    use crate::llm::tool::{Tool, ToolDefinition};

    struct StrictTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        tool_name: String,
    }
    impl Tool for StrictTool {
        const NAME: &'static str = "describe_tool";
        type Error = StrictToolError;
        type Args = StrictArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
            panic!("the tool must not run on non-object arguments");
        }
    }

    let (node, hook) = test_hook().await;
    ready_hook_for(&hook).await;

    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "describe_tool".to_string(),
                serde_json::json!([]),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictTool)];

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("describe"),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);
    while let Some(item) = stream.next().await {
        item.expect("loop must not fail; non-object args are notified, not raised");
    }

    // The started call terminalized failed/argumentInvalid — never a live
    // completed call carrying poison (#589's persist gate).
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state tool_failure_class } }")
        .await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|v| v.as_str()) == Some("describe_tool")
                && row.get("lifecycle_state").and_then(|v| v.as_str()) == Some("failed")
                && row.get("tool_failure_class").and_then(|v| v.as_str()) == Some("argumentInvalid")
        }),
        "a non-object-args call must terminalize failed/argumentInvalid, got: {rows:?}"
    );

    // The next provider request carries object-shaped args — the [] never
    // re-egresses.
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_all_history_tool_args_object_shaped(&histories[1]);
}

/// Every tool call inside a (native) history must carry object-shaped
/// arguments — the provider-render precondition (#590).
fn assert_all_history_tool_args_object_shaped(history: &[Message]) {
    for message in history {
        if let Message::Assistant { content, .. } = message {
            for item in content {
                if let AssistantContent::ToolCall(tool_call) = item {
                    assert!(
                        tool_call.function.arguments.is_object(),
                        "non-object tool-call arguments reached the provider: {:?}",
                        tool_call.function.arguments
                    );
                }
            }
        }
    }
}
