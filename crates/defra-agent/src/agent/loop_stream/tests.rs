use std::sync::Arc;

use futures::{stream, StreamExt};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Message, ToolDefinition,
};
use rig::streaming::{RawStreamingChoice, StreamedAssistantContent, StreamingCompletionResponse};
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;

use super::*;
use crate::ensure_schemas;
use crate::hook::{DefraSessionHook, FailurePolicy};

/// A `CompletionModel` whose `stream` replays a fixed script of
/// `RawStreamingChoice`s, so loop behavior can be tested without a provider.
#[derive(Clone)]
struct ScriptedModel {
    chunks: Arc<Vec<RawStreamingChoice<()>>>,
}

impl ScriptedModel {
    fn new(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self {
            chunks: Arc::new(chunks),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for ScriptedModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self {
            chunks: Arc::new(Vec::new()),
        }
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
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
            self.chunks.iter().cloned().map(Ok).collect();
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
