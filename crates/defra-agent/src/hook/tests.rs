use std::sync::Arc;

use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResult,
    ToolResultContent, UserContent,
};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use rig::one_or_many::OneOrMany;
use rig::streaming::StreamingCompletionResponse;
use serde_json::json;

use super::*;
use crate::ensure_schemas;

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
            "completion is unused in hook tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in hook tests".to_string(),
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

fn session_state_for_test() -> SessionState {
    SessionState {
        session_id: Some("session-1".to_string()),
        current_request_id: None,
        agent_name: "agent".to_string(),
        sequence: 0,
        transcript_turn: TranscriptTurnState::Idle,
        persisted_tool_result_ids: std::collections::HashSet::new(),
        initialized: true,
    }
}

#[test]
fn transcript_turn_state_allocates_new_assistant_after_saved_turn() {
    let mut state = session_state_for_test();

    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert_eq!(state.persist_assistant_turn().unwrap(), 1);
    assert!(state.mark_stream_tool_result_seen("call-1").unwrap());
    assert!(!state.mark_stream_tool_result_seen("call-1").unwrap());

    state.reset_after_user_message();
    assert_eq!(state.begin_or_continue_assistant_turn(), 2);
    assert_eq!(state.persist_assistant_turn().unwrap(), 2);
}

#[test]
fn transcript_turn_state_rejects_stream_result_before_assistant_is_saved() {
    let mut state = session_state_for_test();

    assert!(state.mark_stream_tool_result_seen("call-1").is_err());
    assert_eq!(state.begin_or_continue_assistant_turn(), 1);
    assert!(state.mark_stream_tool_result_seen("call-1").is_err());
    assert_eq!(state.persist_assistant_turn().unwrap(), 1);
    assert!(state.mark_stream_tool_result_seen("call-1").unwrap());
}

async fn create_streaming_response(
    node: &defra_node::EmbeddedNode,
    request_id: &str,
    session_id: &str,
) {
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{request_id}",
                request_id: "{request_id}",
                agent_did: "did:defra-agent:general",
                behavior_id: "general",
                session_id: "{session_id}",
                content: "",
                reasoning: "",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 0,
                created_at: "2026-04-21T00:00:00Z",
                completed_at: ""
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create response failed: {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn streaming_turn_persists_full_assistant_history_in_sequence() {
    let data_path =
        std::env::temp_dir().join(format!("agent-daemon-hook-{}", uuid::Uuid::new_v4()));
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
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let user_prompt = user_text_message("Inspect /tmp/main.rs");
    assert!(matches!(
        PromptHook::<TestModel>::on_completion_call(&hook, &user_prompt, &[]).await,
        HookAction::Continue
    ));

    let tool_args = r#"{"file_path":"/tmp/main.rs"}"#;
    assert!(matches!(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "read",
            Some("call-1".to_string()),
            "internal-1",
            tool_args,
        )
        .await,
        ToolCallHookAction::Continue
    ));

    assert!(matches!(
        PromptHook::<TestModel>::on_tool_result(
            &hook,
            "read",
            Some("call-1".to_string()),
            "internal-1",
            tool_args,
            "fn main() {}\n",
        )
        .await,
        HookAction::Continue
    ));

    let streamed_assistant_turn = Message::Assistant {
        id: None,
        content: OneOrMany::many(vec![
            AssistantContent::Reasoning(
                Reasoning::new("Need to inspect the file first").with_id("rs_1".to_string()),
            ),
            AssistantContent::ToolCall(ToolCall {
                id: "internal-1".to_string(),
                call_id: Some("call-1".to_string()),
                function: ToolFunction {
                    name: "read".to_string(),
                    arguments: json!({ "file_path": "/tmp/main.rs" }),
                },
                signature: None,
                additional_params: None,
            }),
            AssistantContent::Text(Text {
                text: "I'm reading the file now.".to_string(),
            }),
        ])
        .unwrap(),
    };
    hook.persist_message(&streamed_assistant_turn)
        .await
        .unwrap();

    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "internal-1".to_string(),
            call_id: Some("call-1".to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: "ephemeral stream payload".to_string(),
            })),
        },
        "internal-1",
    )
    .await
    .unwrap();

    hook.persist_message(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text {
            text: "The file looks healthy.".to_string(),
        })),
    })
    .await
    .unwrap();

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 4);

    assert!(matches!(
        &history[0],
        Message::User { content }
            if matches!(content.first_ref(), UserContent::Text(Text { text }) if text == "Inspect /tmp/main.rs")
    ));
    assert!(matches!(
        &history[1],
        Message::Assistant { content, .. }
            if content.len() == 3
                && matches!(content.first_ref(), AssistantContent::Reasoning(reasoning) if reasoning.id.as_deref() == Some("rs_1"))
                && matches!(content.iter().nth(1), Some(AssistantContent::ToolCall(tool_call)) if tool_call.call_id.as_deref() == Some("call-1"))
                && matches!(content.iter().nth(2), Some(AssistantContent::Text(Text { text })) if text == "I'm reading the file now.")
    ));
    assert!(matches!(
        &history[2],
        Message::User { content }
            if matches!(content.first_ref(), UserContent::ToolResult(tool_result)
                if tool_result.call_id.as_deref() == Some("call-1")
                    && matches!(tool_result.content.first_ref(), ToolResultContent::Text(Text { text }) if text == "fn main() {}\n"))
    ));
    assert!(matches!(
        &history[3],
        Message::Assistant { content, .. }
            if matches!(content.first_ref(), AssistantContent::Text(Text { text }) if text == "The file looks healthy.")
    ));

    let resp = node
        .execute(&format!(
            r#"{{
                    AgentToolCall(
                        filter: {{
                            session_id: {{ _eq: "{session_id}" }},
                            tool_call_id: {{ _eq: "internal-1" }}
                        }},
                        limit: 1
                    ) {{
                        message_sequence
                        result
                        status
                    }}
                }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query tool call failed: {:?}",
        resp.errors
    );

    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("tool call row");

    assert_eq!(
        row.get("message_sequence").and_then(|value| value.as_u64()),
        Some(2)
    );
    assert_eq!(
        row.get("result").and_then(|value| value.as_str()),
        Some("fn main() {}\n")
    );
    assert_eq!(
        row.get("status").and_then(|value| value.as_str()),
        Some("completed")
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn tool_call_after_saved_assistant_starts_new_turn_without_orphan_result() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-tool-turn-{}", uuid::Uuid::new_v4()));
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
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let user_prompt = user_text_message("Inspect mini-1");
    assert!(matches!(
        PromptHook::<TestModel>::on_completion_call(&hook, &user_prompt, &[]).await,
        HookAction::Continue
    ));

    assert!(matches!(
        PromptHook::<TestModel>::on_tool_call(&hook, "first", None, "internal-1", "{}").await,
        ToolCallHookAction::Continue
    ));
    hook.persist_message(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: "call-1".to_string(),
            call_id: None,
            function: ToolFunction {
                name: "first".to_string(),
                arguments: json!({}),
            },
            signature: None,
            additional_params: None,
        })),
    })
    .await
    .unwrap();

    assert!(matches!(
        PromptHook::<TestModel>::on_tool_call(&hook, "second", None, "internal-2", "{}").await,
        ToolCallHookAction::Continue
    ));
    assert!(matches!(
        PromptHook::<TestModel>::on_tool_result(
            &hook,
            "second",
            Some("call-2".to_string()),
            "internal-2",
            "{}",
            "second result",
        )
        .await,
        HookAction::Continue
    ));

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(
        history.len(),
        2,
        "tool result must not be persisted before its assistant turn"
    );

    let resp = node
        .execute(&format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_call_id: {{ _eq: "internal-2" }}
                    }},
                    limit: 1
                ) {{ message_sequence result status }}
            }}"#
        ))
        .await;
    assert!(
        !resp.has_errors(),
        "query tool call failed: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("tool call row");
    assert_eq!(
        row.get("message_sequence").and_then(|value| value.as_u64()),
        Some(3)
    );
    assert_eq!(
        row.get("result").and_then(|value| value.as_str()),
        Some("second result")
    );
    assert_eq!(
        row.get("status").and_then(|value| value.as_str()),
        Some("completed")
    );

    hook.persist_message(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: "call-2".to_string(),
            call_id: None,
            function: ToolFunction {
                name: "second".to_string(),
                arguments: json!({}),
            },
            signature: None,
            additional_params: None,
        })),
    })
    .await
    .unwrap();
    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "call-2".to_string(),
            call_id: None,
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: "stream fallback".to_string(),
            })),
        },
        "internal-2",
    )
    .await
    .unwrap();

    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 4);
    assert!(matches!(
        &history[2],
        Message::Assistant { content, .. }
            if matches!(content.first_ref(), AssistantContent::ToolCall(tool_call)
                if tool_call.id == "call-2")
    ));
    assert!(matches!(
        &history[3],
        Message::User { content }
            if matches!(content.first_ref(), UserContent::ToolResult(tool_result)
                if tool_result.id == "call-2"
                    && matches!(tool_result.content.first_ref(), ToolResultContent::Text(Text { text }) if text == "second result"))
    ));

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn completion_response_marks_agent_response_as_materialized() {
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-materialized-{}", uuid::Uuid::new_v4()));
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
        "did:defra-agent:general",
        FailurePolicy::default(),
    );
    let user_prompt = user_text_message("Explain the runtime model");
    assert!(matches!(
        PromptHook::<TestModel>::on_completion_call(&hook, &user_prompt, &[]).await,
        HookAction::Continue
    ));

    let session_id = hook.session_id().await.expect("session id");
    hook.set_active_request_id(Some("req-materialized".to_string()))
        .await;
    create_streaming_response(&node, "req-materialized", &session_id).await;

    let response = CompletionResponse {
        choice: OneOrMany::one(AssistantContent::Text(Text {
            text: "Here is the answer.".to_string(),
        })),
        usage: Usage::new(),
        raw_response: (),
        message_id: None,
    };

    assert!(matches!(
        PromptHook::<TestModel>::on_completion_response(&hook, &user_prompt, &response).await,
        HookAction::Continue
    ));

    let resp = node
        .execute(
            r#"{
                AgentResponse(filter: { request_id: { _eq: "req-materialized" } }, limit: 1) {
                    materialized_message_sequence
                    materialized_at
                }
            }"#,
        )
        .await;
    assert!(
        !resp.has_errors(),
        "query response failed: {:?}",
        resp.errors
    );

    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("response row");

    assert_eq!(
        row.get("materialized_message_sequence")
            .and_then(|value| value.as_u64()),
        Some(2)
    );
    assert!(row
        .get("materialized_at")
        .and_then(|value| value.as_str())
        .is_some());

    let _ = std::fs::remove_dir_all(&data_path);
}
