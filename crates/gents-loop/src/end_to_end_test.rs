//! Proves the loop is usable with no DefraDB and no network: a scripted
//! in-memory [`CompletionModel`] streams a tool call on turn one and a final
//! answer on turn two, an in-memory [`SessionHook`] records every
//! persistence call, and a plain [`ToolDyn`] tool is dispatched in between.
//! Nothing here touches a socket or a filesystem.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gents_protocol::message::{Message, ToolResult};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};

use crate::backend_provider::BackendProviderKind;
use crate::loop_stream::{run_loop_to_text, LoopConfig};
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use crate::session_hook::SessionHook;
use crate::tool::{Tool, ToolDefinition, ToolDyn};
use crate::tool_call_lifecycle::ToolOutcome;
use crate::{HookAction, ToolCallHookAction};

/// A `CompletionModel` that replays a fixed script of streamed responses, one
/// script entry per `stream` call: no network, no rig provider.
#[derive(Clone, Default)]
struct ScriptedModel {
    turns: Arc<Mutex<Vec<Vec<RawStreamingChoice<()>>>>>,
}

impl ScriptedModel {
    fn new(turns: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into_iter().rev().collect())),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for ScriptedModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        Self::default()
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        unreachable!("the owned loop only ever calls stream()")
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let items = self
            .turns
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop()
            .expect("scripted model called more times than it was scripted for");
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
            items.into_iter().map(Ok).collect();
        Ok(StreamingCompletionResponse::stream(Box::pin(
            futures::stream::iter(items),
        )))
    }
}

/// A tool with no filesystem, socket, or DefraDB access at all: it just
/// echoes its argument back, uppercased.
struct EchoTool {
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(serde::Deserialize)]
struct EchoArgs {
    text: String,
}

impl Tool for EchoTool {
    const NAME: &'static str = "echo";
    type Error = std::convert::Infallible;
    type Args = EchoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Echoes its input, uppercased.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(args.text.clone());
        Ok(args.text.to_uppercase())
    }
}

/// Every persistence call the loop made, in order, with no DefraDB behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedCall {
    CompletionCall { prompt: String },
    ToolCall { tool_name: String },
    ToolResult { tool_name: String, outcome: String },
    PersistMessage { role: &'static str },
}

#[derive(Clone, Default)]
struct RecordingHook {
    log: Arc<Mutex<Vec<RecordedCall>>>,
    sequence: Arc<AtomicUsize>,
}

impl RecordingHook {
    fn log(&self, call: RecordedCall) {
        self.log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(call);
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

#[async_trait]
impl SessionHook for RecordingHook {
    async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        _history: &[Message],
        _context: Option<&Message>,
    ) -> HookAction {
        self.log(RecordedCall::CompletionCall {
            prompt: prompt.rag_text().unwrap_or_default(),
        });
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
    ) -> ToolCallHookAction {
        self.log(RecordedCall::ToolCall {
            tool_name: tool_name.to_string(),
        });
        ToolCallHookAction::Continue
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        outcome: &ToolOutcome,
    ) -> HookAction {
        self.log(RecordedCall::ToolResult {
            tool_name: tool_name.to_string(),
            outcome: outcome.model_facing_text().to_string(),
        });
        HookAction::Continue
    }

    async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> crate::live_output::LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(internal_call_id.to_string())
            .await
    }

    async fn session_id(&self) -> Option<String> {
        Some("in-memory-session".to_string())
    }

    fn apply_persistence_policy(
        &self,
        result: anyhow::Result<()>,
        _context: &str,
    ) -> anyhow::Result<()> {
        result
    }

    async fn persist_message(&self, message: &Message) -> anyhow::Result<u32> {
        let role = match message {
            Message::System { .. } => "system",
            Message::User { .. } => "user",
            Message::Assistant { .. } => "assistant",
        };
        self.log(RecordedCall::PersistMessage { role });
        Ok(self.sequence.fetch_add(1, Ordering::SeqCst) as u32)
    }

    async fn persist_stream_tool_result_message(
        &self,
        _tool_result: &ToolResult,
        _internal_call_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn persist_stream_tool_result_progress(
        &self,
        _tool_result: &ToolResult,
        _internal_call_id: &str,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn persist_inflight_assistant_turn(&self, _message: &Message) -> anyhow::Result<u32> {
        Ok(self.sequence.fetch_add(1, Ordering::SeqCst) as u32)
    }

    async fn mark_current_response_materialized(&self, _sequence: u32) -> anyhow::Result<()> {
        Ok(())
    }

    async fn register_stream_tool_call_identity(
        &self,
        _internal_call_id: &str,
        _result_id: &str,
        _call_id: Option<&str>,
    ) {
    }
}

fn scripted_tool_call() -> RawStreamingChoice<()> {
    RawStreamingChoice::ToolCall(RawStreamingToolCall {
        id: "call_1".to_string(),
        internal_call_id: "call_1".to_string(),
        call_id: Some("call_1".to_string()),
        name: "echo".to_string(),
        arguments: serde_json::json!({"text": "hi"}),
        signature: None,
        additional_params: None,
    })
}

fn test_loop_config() -> LoopConfig {
    LoopConfig {
        provider_input_counter: Arc::new(ProviderInputCounter::new(
            BackendProviderKind::OpenAiCompatible,
            OpenAiWireApi::ChatCompletions,
            "test-model",
        )),
        preamble: Some("You are a helpful assistant.".to_string()),
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
        context_window: 128_000,
        compaction_threshold: 0.75,
        retry_policy: crate::completion_retry::CompletionRetryPolicy::interactive_default(),
        deadline: None,
        max_turns: 8,
        output_obligation_gate: None,
    }
}

#[tokio::test]
async fn the_loop_dispatches_a_tool_and_threads_messages_with_no_defradb_and_no_network() {
    let model = ScriptedModel::new(vec![
        vec![scripted_tool_call(), RawStreamingChoice::FinalResponse(())],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tool_calls = Arc::new(Mutex::new(Vec::new()));
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(EchoTool {
        calls: tool_calls.clone(),
    })]);
    let hook = RecordingHook::default();

    let final_text = run_loop_to_text(
        model,
        Some(hook.clone()),
        Message::user("please echo hi"),
        Vec::new(),
        tools,
        test_loop_config(),
    )
    .await
    .expect("the loop must complete with no DefraDB and no network");

    assert_eq!(final_text, "done");
    assert_eq!(
        tool_calls.lock().unwrap().as_slice(),
        &["hi".to_string()],
        "the echo tool must have been dispatched exactly once"
    );

    // The hook sees the persistence calls in order: the user prompt, the
    // tool call, the tool result, the assistant's tool-call turn, then the
    // final assistant answer.
    let calls = hook.calls();
    assert_eq!(
        calls[0],
        RecordedCall::CompletionCall {
            prompt: "please echo hi".to_string()
        }
    );
    assert_eq!(
        calls[1],
        RecordedCall::ToolCall {
            tool_name: "echo".to_string()
        }
    );
    assert_eq!(
        calls[2],
        RecordedCall::ToolResult {
            tool_name: "echo".to_string(),
            outcome: "HI".to_string(),
        }
    );
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, RecordedCall::PersistMessage { role: "assistant" })),
        "the assistant's tool-call turn must be persisted: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, RecordedCall::CompletionCall { .. }))
            && calls.len() >= 4,
        "a second completion call must follow the tool result: {calls:?}"
    );
}
