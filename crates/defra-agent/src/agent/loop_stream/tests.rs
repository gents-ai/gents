use std::collections::VecDeque;
use std::sync::Arc;

use futures::{stream, StreamExt};
use rig::completion::message::ToolResultContent;
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Message, ToolDefinition,
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
}

impl ScriptedModel {
    fn new(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self::new_turns(vec![chunks])
    }

    fn new_turns(turns: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
        }
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
        let chunks = self
            .turns
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| vec![RawStreamingChoice::FinalResponse(())]);
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
            chunks.into_iter().map(Ok).collect();
        Ok(StreamingCompletionResponse::stream(Box::pin(stream::iter(
            items,
        ))))
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
    let data_path = std::env::temp_dir().join(format!("agent-loop-stream-{}", uuid::Uuid::new_v4()));
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
        hook,
        Message::user("hi"),
        Vec::new(),
        Vec::new(),
        config(0),
    );
    futures::pin_mut!(stream);

    let mut texts = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("loop item should be Ok") {
            LoopStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text)) => {
                texts.push(text.text);
            }
            LoopStreamItem::FinalResponse(final_response) => {
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

    // Establish the request context on_tool_call needs (session + request id).
    let prompt = Message::user("use the echo tool");
    assert!(matches!(
        PromptHook::<ScriptedModel>::on_completion_call(&hook, &prompt, &[]).await,
        HookAction::Continue
    ));
    let request_id = uuid::Uuid::new_v4().to_string();
    hook.set_active_request_id(Some(request_id)).await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    let session_id = hook.session_id().await.expect("session id");

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

    let stream = run_loop_stream(model, hook, prompt, Vec::new(), tools, config(4));
    futures::pin_mut!(stream);

    let mut tool_results = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("loop item should be Ok") {
            LoopStreamItem::StreamUserItem(StreamedUserContent::ToolResult { tool_result, .. }) => {
                tool_results.push(tool_result_text(&tool_result.content.first()).to_string());
            }
            LoopStreamItem::FinalResponse(final_response) => {
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
    let _ = &session_id;
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state result } }")
        .await;
    assert!(!resp.has_errors(), "AgentToolCall query failed: {:?}", resp.errors);
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
