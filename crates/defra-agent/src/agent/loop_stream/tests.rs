use std::collections::VecDeque;
use std::sync::Arc;

use futures::{stream, StreamExt};
use rig::completion::message::{AssistantContent, ToolResultContent, UserContent};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Message,
    ToolDefinition,
};
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent, StreamedUserContent,
    StreamingCompletionResponse,
};
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use tokio::sync::Mutex;

use super::*;
use crate::ensure_schemas;
use crate::hook::{DefraSessionHook, FailurePolicy};

/// A `CompletionModel` whose `stream` replays one scripted turn per call: each
/// `stream()` pops the next `Vec<RawStreamingChoice>` from the queue, letting a
/// test drive a multi-turn (tool-call then text) loop without a provider. Once
/// the queue is empty it yields a bare final response so the loop terminates.
#[derive(Clone)]
struct ScriptedModel {
    turns: Arc<Mutex<VecDeque<Vec<RawStreamingChoice<()>>>>>,
    /// `chat_history` of every request the loop sent, in order — lets a test
    /// assert how the loop threaded prior turns back to the provider.
    seen_histories: Arc<Mutex<Vec<OneOrMany<Message>>>>,
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
            stall_after_chunks: false,
        }
    }

    /// A single turn that emits `chunks` then stalls forever instead of ending.
    fn new_stalling(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        let mut model = Self::new_turns(vec![chunks]);
        model.stall_after_chunks = true;
        model
    }

    async fn seen_histories(&self) -> Vec<OneOrMany<Message>> {
        self.seen_histories.lock().await.clone()
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
        self.seen_histories
            .lock()
            .await
            .push(_request.chat_history.clone());
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

    fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "echo".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
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

    fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "fixed".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
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

    fn definition<'a>(&'a self, prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
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

    fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
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
        temperature: None,
        max_tokens: None,
        additional_params: None,
        tool_choice: None,
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
                tool_results.push(tool_result_text(&tool_result.content.first()).to_string());
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
            bounded_len = Some(tool_result_text(&tool_result.content.first()).len());
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
                    if tool_result_text(&result.content.first()) == "ECHOED")))),
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
