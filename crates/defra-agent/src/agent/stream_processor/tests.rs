use std::sync::Arc;
use std::time::Duration;

use rig::agent::{HookAction, MultiTurnStreamItem, PromptHook};
use rig::completion::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResult,
    ToolResultContent, UserContent,
};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::one_or_many::OneOrMany;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingCompletionResponse};

use super::*;
use crate::ensure_schemas;
use crate::hook::FailurePolicy;
use crate::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use crate::streaming::DefraStreamWriter;
use crate::watcher::AgentRequest;

#[derive(Clone, Default)]
struct TestModel;

#[allow(refining_impl_trait)]
impl CompletionModel for TestModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in stream processor tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in stream processor tests".to_string(),
        ))
    }
}

fn user_text_message(text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: text.to_string(),
        })),
    }
}

#[tokio::test]
async fn persist_partial_turn_saves_reasoning_and_text_to_history() {
    let data_path =
        std::env::temp_dir().join(format!("agent-stream-processor-{}", uuid::Uuid::new_v4()));
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
    assert!(matches!(
        PromptHook::<TestModel>::on_completion_call(
            &hook,
            &user_text_message("Inspect the repo"),
            &[]
        )
        .await,
        HookAction::Continue
    ));

    let session_id = hook.session_id().await.expect("session id");
    let request_id = uuid::Uuid::new_v4().to_string();
    let request = AgentRequest {
        doc_id: "request-doc".to_string(),
        request_id: request_id.clone(),
        agent_did: "did:defra-agent:test".to_string(),
        behavior_id: Some("general".to_string()),
        session_id: session_id.clone(),
        content: "Inspect the repo".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "test-agent",
        "did:defra-agent:test",
        request,
        30,
        crate::lifecycle::ExecutionOrigin::Interactive,
        "test-backend",
    );
    let stream_writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    // Begin a streaming response so reset_tail (called by persist_partial_turn)
    // has a live buffer to clear.
    let response_doc_id = stream_writer
        .begin(&session_id, &request_id, "general")
        .await
        .unwrap();
    let mut processor =
        StreamProcessor::new(&hook, &stream_writer, &mut lifecycle, &response_doc_id);

    processor.assistant_turn.push_reasoning(
        Reasoning::new("Need to inspect directory structure first")
            .with_id("rs_partial".to_string()),
    );
    processor
        .assistant_turn
        .push_text("I started by checking the repo layout.");

    assert!(processor.has_observable_activity());
    assert!(processor
        .persist_partial_turn("persist errored assistant turn")
        .await
        .unwrap());

    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert!(matches!(
        &history[1],
        Message::Assistant { content, .. }
            if content.len() == 2
                && matches!(content.first_ref(), AssistantContent::Reasoning(reasoning)
                    if reasoning.id.as_deref() == Some("rs_partial"))
                && matches!(content.iter().nth(1), Some(AssistantContent::Text(Text { text }))
                    if text == "I started by checking the repo layout.")
    ));

    let _ = std::fs::remove_dir_all(&data_path);
}

// ---------------------------------------------------------------------------
// Helpers for the tail-reset integration test
// ---------------------------------------------------------------------------

async fn create_pending_request(
    node: &Arc<defra_node::EmbeddedNode>,
    request_id: &str,
    session_id: &str,
) -> String {
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "did:defra-agent:test",
                behavior_id: "general",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "test prompt",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentRequest failed: {:?}",
        resp.errors
    );
    // DefraDB returns the doc id in the mutation response or we query for it.
    if let Some(doc_id) = resp
        .data
        .as_ref()
        .and_then(|d| d.get("create_AgentRequest"))
        .and_then(|v| v.get("_docID"))
        .and_then(|v| v.as_str())
    {
        return doc_id.to_string();
    }
    // Fallback: query by request_id.
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentRequest lookup failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .expect("request _docID")
}

async fn load_response_doc(
    node: &defra_node::EmbeddedNode,
    doc_id: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                _docID
                content
                reasoning
                status
                token_count
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "load_response_doc failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentResponse"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.as_object())
        .cloned()
        .expect("AgentResponse row")
}

fn text_item(text: &str) -> Result<MultiTurnStreamItem<()>, rig::agent::StreamingError> {
    Ok(MultiTurnStreamItem::StreamAssistantItem(
        StreamedAssistantContent::Text(Text {
            text: text.to_string(),
        }),
    ))
}

fn tool_call_item(
    name: &str,
    args_json: &str,
    internal_id: &str,
) -> Result<MultiTurnStreamItem<()>, rig::agent::StreamingError> {
    tool_call_item_with_ids(name, args_json, internal_id, internal_id, None)
}

fn tool_call_item_with_ids(
    name: &str,
    args_json: &str,
    tool_id: &str,
    internal_id: &str,
    call_id: Option<&str>,
) -> Result<MultiTurnStreamItem<()>, rig::agent::StreamingError> {
    Ok(MultiTurnStreamItem::StreamAssistantItem(
        StreamedAssistantContent::ToolCall {
            tool_call: ToolCall {
                id: tool_id.to_string(),
                call_id: call_id.map(ToOwned::to_owned),
                function: ToolFunction {
                    name: name.to_string(),
                    arguments: serde_json::from_str(args_json).unwrap(),
                },
                signature: None,
                additional_params: None,
            },
            internal_call_id: internal_id.to_string(),
        },
    ))
}

fn tool_result_item(
    tool_id: &str,
    result_json: &str,
    internal_id: &str,
) -> Result<MultiTurnStreamItem<()>, rig::agent::StreamingError> {
    tool_result_item_with_call_id(tool_id, None, result_json, internal_id)
}

fn tool_result_item_with_call_id(
    tool_id: &str,
    call_id: Option<&str>,
    result_json: &str,
    internal_id: &str,
) -> Result<MultiTurnStreamItem<()>, rig::agent::StreamingError> {
    Ok(MultiTurnStreamItem::StreamUserItem(
        StreamedUserContent::ToolResult {
            tool_result: ToolResult {
                id: tool_id.to_string(),
                call_id: call_id.map(ToOwned::to_owned),
                content: OneOrMany::one(ToolResultContent::Text(Text {
                    text: result_json.to_string(),
                })),
            },
            internal_call_id: internal_id.to_string(),
        },
    ))
}

fn final_item(response_text: &str) -> Result<MultiTurnStreamItem<()>, rig::agent::StreamingError> {
    Ok(MultiTurnStreamItem::<()>::final_response(
        response_text,
        rig::completion::Usage::new(),
    ))
}

#[tokio::test]
async fn hook_persisted_tool_result_dedupes_matching_stream_result() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-tool-dedupe-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = crate::hook::DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        PromptHook::<TestModel>::on_completion_call(
            &hook,
            &user_text_message("discover available tools"),
            &[]
        )
        .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    let request = AgentRequest {
        doc_id: request_doc_id,
        request_id: request_id.clone(),
        agent_did: "did:defra-agent:test".to_string(),
        behavior_id: Some("general".to_string()),
        session_id: session_id.clone(),
        content: "discover available tools".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:defra-agent:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let stream_writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_millis(0),
    );
    let response_doc_id = stream_writer
        .begin(&session_id, &request_id, "general")
        .await
        .unwrap();
    lifecycle.set_response_doc_id(&response_doc_id);

    let mut processor =
        StreamProcessor::new(&hook, &stream_writer, &mut lifecycle, &response_doc_id);

    let stored_call_id = "OaoTQYzCdoptKiK_mdhBA";
    let model_result_id = "c6b8bdeb-ab92-4481-b763-bdafbd463904";
    let tool_args = r#"{"tool":"discover_tools"}"#;
    let tool_result = r#"{"tools":["discover_tools","describe_tool"]}"#;

    processor
        .process_item(tool_call_item_with_ids(
            "discover_tools",
            tool_args,
            model_result_id,
            model_result_id,
            Some(model_result_id),
        ))
        .await
        .unwrap();
    assert!(matches!(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "discover_tools",
            Some(model_result_id.to_string()),
            stored_call_id,
            tool_args,
        )
        .await,
        rig::agent::ToolCallHookAction::Continue
    ));
    assert!(processor
        .persist_partial_turn("persist streamed assistant tool call")
        .await
        .unwrap());
    assert!(matches!(
        PromptHook::<TestModel>::on_tool_result(
            &hook,
            "discover_tools",
            Some(model_result_id.to_string()),
            stored_call_id,
            tool_args,
            tool_result,
        )
        .await,
        HookAction::Continue
    ));

    processor
        .process_item(tool_result_item_with_call_id(
            model_result_id,
            Some(model_result_id),
            tool_result,
            model_result_id,
        ))
        .await
        .unwrap();

    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    let tool_results = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => match content.first_ref() {
                UserContent::ToolResult(tool_result) => Some(tool_result),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        tool_results.len(),
        1,
        "hook and stream paths must materialize one logical tool result"
    );
    assert_eq!(tool_results[0].id, model_result_id);
    assert_eq!(tool_results[0].call_id.as_deref(), Some(model_result_id));
    assert!(matches!(
        tool_results[0].content.first_ref(),
        ToolResultContent::Text(Text { text }) if text == tool_result
    ));
    assert_eq!(
        crate::session::load_tool_call_result(&node, &session_id, stored_call_id)
            .await
            .unwrap(),
        tool_result
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn post_tool_resumed_resets_response_tail() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-tool-reset-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    // Set up session hook + establish session by persisting user message.
    let hook = crate::hook::DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        PromptHook::<TestModel>::on_completion_call(&hook, &user_text_message("test prompt"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    // Create a pending request in the DB so the lifecycle can be claimed.
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;

    let request = AgentRequest {
        doc_id: request_doc_id.clone(),
        request_id: request_id.clone(),
        agent_did: "did:defra-agent:test".to_string(),
        behavior_id: Some("general".to_string()),
        session_id: session_id.clone(),
        content: "test prompt".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:defra-agent:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );

    // Claim → Streaming so advance() calls will work.
    let outcome = lifecycle.claim().await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Claimed, "expected Claimed outcome");

    // Use 0 ms batch interval so write_tokens flushes immediately to DB.
    let stream_writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_millis(0),
    );
    let response_doc_id = stream_writer
        .begin(&session_id, &request_id, "general")
        .await
        .unwrap();
    lifecycle.set_response_doc_id(&response_doc_id);

    let mut processor =
        StreamProcessor::new(&hook, &stream_writer, &mut lifecycle, &response_doc_id);

    // Feed: Text → Text → ToolCall → ToolResult
    processor.process_item(text_item("hello ")).await.unwrap();
    processor.process_item(text_item("world")).await.unwrap();
    processor
        .process_item(tool_call_item("search", r#"{"q":"x"}"#, "call-1"))
        .await
        .unwrap();
    processor
        .process_item(tool_result_item("call-1", r#"{"hit":1}"#, "call-1"))
        .await
        .unwrap();

    // After ToolResult: tail must be reset to empty.
    let after_tool = load_response_doc(&node, &response_doc_id).await;
    assert_eq!(
        after_tool["content"].as_str(),
        Some(""),
        "content must be reset after tool-result persisted"
    );
    assert_eq!(
        after_tool["reasoning"].as_str(),
        Some(""),
        "reasoning must be reset after tool-result persisted"
    );

    // Feed: Text("done") after the tool boundary.
    processor.process_item(text_item("done")).await.unwrap();

    // The new text is live in the tail.
    let after_resume = load_response_doc(&node, &response_doc_id).await;
    assert_eq!(
        after_resume["content"].as_str(),
        Some("done"),
        "post-boundary text must appear in fresh tail"
    );

    // Feed: FinalResponse.
    processor.process_item(final_item("done")).await.unwrap();

    // After FinalResponse the tail is cleared again.
    let after_final = load_response_doc(&node, &response_doc_id).await;
    assert_eq!(
        after_final["content"].as_str(),
        Some(""),
        "content must be cleared after final-response persisted"
    );
    assert_eq!(
        after_final["reasoning"].as_str(),
        Some(""),
        "reasoning must be cleared after final-response persisted"
    );

    let _ = std::fs::remove_dir_all(&data_path);
}
