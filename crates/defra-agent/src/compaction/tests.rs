use std::sync::{Arc, Mutex};

use rig::completion::message::{
    AssistantContent, Message, Text, ToolCall, ToolResult, ToolResultContent, UserContent,
};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use rig::one_or_many::OneOrMany;
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};

use super::*;
use crate::ensure_schemas;
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::session;

fn text_msg(role: &str, text: &str) -> Message {
    match role {
        "user" => Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: text.to_string(),
            })),
        },
        "assistant" => Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
        },
        _ => panic!("unknown role"),
    }
}

fn tool_call_msg(name: &str, args: &str) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: "call-1".to_string(),
            call_id: Some("call-1".to_string()),
            function: rig::completion::message::ToolFunction {
                name: name.to_string(),
                arguments: serde_json::from_str(args).unwrap_or_default(),
            },
            signature: None,
            additional_params: None,
        })),
    }
}

fn tool_result_msg(call_id: &str, result_text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: call_id.to_string(),
            call_id: Some(call_id.to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: result_text.to_string(),
            })),
        })),
    }
}

fn tool_call_content(id: &str) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.to_string(),
        call_id: Some(id.to_string()),
        function: rig::completion::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    })
}

#[test]
fn drop_unpaired_tool_calls_removes_calls_without_results() {
    // #445: assistant turn has text + a paired call (call-A, has a result) + an
    // unpaired call (call-B, no result). The unpaired call must be dropped before
    // the provider sees it; text and the paired call (with its result) survive.
    let messages = vec![
        Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::Text(Text {
                    text: "thinking".to_string(),
                }),
                tool_call_content("call-A"),
                tool_call_content("call-B"),
            ])
            .unwrap(),
        },
        tool_result_msg("call-A", "A-result"),
    ];

    let out = super::drop_unpaired_tool_calls(messages);

    assert_eq!(
        out.len(),
        2,
        "assistant turn + its one paired result remain"
    );
    let kept_calls: Vec<String> = match &out[0] {
        Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::ToolCall(tool_call) => Some(tool_call.id.clone()),
                _ => None,
            })
            .collect(),
        other => panic!("expected assistant message, got {other:?}"),
    };
    assert_eq!(
        kept_calls,
        vec!["call-A".to_string()],
        "unpaired call-B must be dropped, paired call-A kept"
    );
    assert!(
        matches!(&out[0], Message::Assistant { content, .. }
            if content.iter().any(|c| matches!(c, AssistantContent::Text(_)))),
        "text content must be preserved"
    );
    assert!(
        matches!(&out[1], Message::User { .. }),
        "result must remain"
    );
}

#[test]
fn drop_unpaired_tool_calls_drops_call_only_assistant_message() {
    // An assistant turn that is nothing but an unpaired tool call is dropped
    // entirely (no dangling call reaches the provider).
    let messages = vec![
        text_msg("user", "go"),
        Message::Assistant {
            id: None,
            content: OneOrMany::one(tool_call_content("call-X")),
        },
    ];
    let out = super::drop_unpaired_tool_calls(messages);
    assert_eq!(
        out.len(),
        1,
        "the all-unpaired assistant message is dropped"
    );
    assert!(matches!(&out[0], Message::User { .. }));
}

#[test]
fn normalize_assistant_content_order_moves_text_before_tool_calls() {
    // Transcripts persisted before the ordering fix can carry assistant text
    // AFTER tool calls; strict providers reject that on reload. Normalization at
    // the provider-send boundary must reorder to (text, reasoning, tool calls)
    // while preserving ids and per-category order.
    let messages = vec![
        Message::Assistant {
            id: Some("msg-1".to_string()),
            content: OneOrMany::many(vec![
                AssistantContent::Reasoning(rig::completion::message::Reasoning::new("why")),
                tool_call_content("call-A"),
                tool_call_content("call-B"),
                AssistantContent::Text(Text {
                    text: "answer".to_string(),
                }),
            ])
            .unwrap(),
        },
        tool_result_msg("call-A", "A-result"),
    ];

    let out = super::normalize_assistant_content_order(messages);

    let (id, kinds): (Option<String>, Vec<&'static str>) = match &out[0] {
        Message::Assistant { id, content } => (
            id.clone(),
            content
                .iter()
                .map(|item| match item {
                    AssistantContent::Text(_) => "text",
                    AssistantContent::Reasoning(_) => "reasoning",
                    AssistantContent::ToolCall(_) => "tool_call",
                    _ => "other",
                })
                .collect(),
        ),
        other => panic!("expected assistant message, got {other:?}"),
    };
    assert_eq!(
        id.as_deref(),
        Some("msg-1"),
        "provider message id preserved"
    );
    assert_eq!(
        kinds,
        vec!["text", "reasoning", "tool_call", "tool_call"],
        "text must lead, tool calls must trail"
    );
    let call_ids: Vec<String> = match &out[0] {
        Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::ToolCall(tool_call) => Some(tool_call.id.clone()),
                _ => None,
            })
            .collect(),
        _ => unreachable!(),
    };
    assert_eq!(
        call_ids,
        vec!["call-A".to_string(), "call-B".to_string()],
        "tool-call relative order preserved"
    );
    assert!(
        matches!(&out[1], Message::User { .. }),
        "non-assistant messages pass through"
    );
}

#[test]
fn normalize_assistant_content_order_is_identity_when_already_ordered() {
    let messages = vec![
        text_msg("user", "go"),
        Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::Text(Text {
                    text: "answer".to_string(),
                }),
                tool_call_content("call-A"),
            ])
            .unwrap(),
        },
        tool_result_msg("call-A", "A-result"),
    ];
    let out = super::normalize_assistant_content_order(messages.clone());
    assert_eq!(out, messages);
}

#[test]
fn drop_unpaired_tool_calls_is_identity_when_all_paired() {
    let messages = vec![
        Message::Assistant {
            id: None,
            content: OneOrMany::one(tool_call_content("call-A")),
        },
        tool_result_msg("call-A", "A-result"),
        text_msg("user", "next"),
    ];
    let out = super::drop_unpaired_tool_calls(messages.clone());
    assert_eq!(
        out.len(),
        messages.len(),
        "fully-paired history must pass through unchanged"
    );
}

#[derive(Clone, Default)]
struct MockSummaryModel {
    response: String,
    last_request: Arc<Mutex<Option<CompletionRequest>>>,
}

impl MockSummaryModel {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            last_request: Arc::new(Mutex::new(None)),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for MockSummaryModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self::default()
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        *self.last_request.lock().unwrap() = Some(request);
        Ok(CompletionResponse {
            choice: OneOrMany::one(AssistantContent::Text(Text {
                text: self.response.clone(),
            })),
            usage: Usage::new(),
            raw_response: (),
            message_id: None,
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        // Compaction now summarizes via the owned loop (#400), which uses
        // `stream`; replay the scripted summary as a single text chunk.
        *self.last_request.lock().unwrap() = Some(request);
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> = vec![
            Ok(RawStreamingChoice::Message(self.response.clone())),
            Ok(RawStreamingChoice::FinalResponse(())),
        ];
        Ok(StreamingCompletionResponse::stream(Box::pin(
            futures::stream::iter(items),
        )))
    }
}

#[test]
fn strip_preserves_text_messages() {
    let messages = vec![text_msg("user", "hello"), text_msg("assistant", "hi there")];
    let (stripped, files) = strip_tool_results(messages);
    assert_eq!(stripped.len(), 2);
    assert!(files.files_read.is_empty());
    assert!(files.files_modified.is_empty());
}

#[test]
fn strip_rewrites_tool_results_into_stubs() {
    let long_result = "x".repeat(5000);
    let messages = vec![
        text_msg("user", "read this file"),
        tool_call_msg("read", r#"{"file_path": "/tmp/test.rs"}"#),
        tool_result_msg("call-1", &long_result),
        text_msg("assistant", "I saw the file"),
    ];

    let (stripped, files) = strip_tool_results(messages);
    assert_eq!(stripped.len(), 4);
    assert_eq!(files.files_read, vec!["/tmp/test.rs"]);
    assert!(files.files_modified.is_empty());

    if let Message::User { content } = &stripped[2] {
        if let UserContent::ToolResult(tr) = content.first_ref() {
            if let ToolResultContent::Text(text) = tr.content.first_ref() {
                assert_eq!(
                    text.text,
                    "[tool: read, call_id: call-1, 5000 bytes — see DefraDB AgentToolCall for full output]"
                );
            } else {
                panic!("expected text content");
            }
        } else {
            panic!("expected tool result");
        }
    } else {
        panic!("expected user message");
    }
}

#[test]
fn strip_extracts_read_and_modified_files() {
    let messages = vec![
        tool_call_msg("read", r#"{"file_path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
        tool_call_msg("write", r#"{"file_path": "/src/lib.rs"}"#),
        tool_result_msg("call-1", "ok"),
    ];

    let (_, files) = strip_tool_results(messages);
    assert_eq!(files.files_read, vec!["/src/main.rs"]);
    assert_eq!(files.files_modified, vec!["/src/lib.rs"]);
}

#[test]
fn needs_compaction_under_threshold() {
    let messages = vec![text_msg("user", "hi")];
    assert!(!needs_compaction(&messages, 100000, 0.75));
}

#[test]
fn needs_compaction_over_threshold() {
    let big = "x".repeat(10000);
    let messages = vec![text_msg("user", &big)];
    assert!(needs_compaction(&messages, 1000, 0.75));
}

#[test]
fn estimate_tokens_rough() {
    assert_eq!(estimate_tokens("hello world!"), 3);
    assert_eq!(estimate_tokens(""), 0);
}

#[tokio::test]
async fn integration_compaction_persists_entry_and_prompt_builder_uses_it() {
    let data_path =
        std::env::temp_dir().join(format!("agent-daemon-compactor-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();
    session::create_session_with_id(&node, "session-1", "general")
        .await
        .unwrap();

    let model = MockSummaryModel::new(
        &serde_json::json!({
            "summary": "The agent repeatedly inspected the source files.",
            "files_read": ["/workspace/main.rs"],
            "files_modified": [],
            "key_decisions": ["Use compaction to collapse older turns"],
            "pending_questions": []
        })
        .to_string(),
    );
    let config = crate::agent::loop_stream::LoopConfig {
        preamble: Some("You are a helpful coding agent.".to_string()),
        temperature: None,
        max_tokens: None,
        additional_params: None,
        tool_choice: None,
        max_turns: 0,
    };
    let compactor = DefraCompactor::new(std::sync::Arc::new(model), config);

    let mut sequence = 1;
    for turn in 0..55 {
        let user = Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: format!("Request {turn}: {}", "x".repeat(800)),
            })),
        };
        let assistant_tool_call = tool_call_msg("read", r#"{"file_path": "/workspace/main.rs"}"#);
        let tool_result = tool_result_msg("call-1", &"file contents\n".repeat(50));
        let assistant = text_msg(
            "assistant",
            &format!("Response {turn}: {}", "y".repeat(500)),
        );

        session::save_message(
            &node,
            "session-1",
            sequence,
            "user",
            &serde_json::to_string(&user).unwrap(),
        )
        .await
        .unwrap();
        sequence += 1;

        session::save_message(
            &node,
            "session-1",
            sequence,
            "assistant",
            &serde_json::to_string(&assistant_tool_call).unwrap(),
        )
        .await
        .unwrap();
        sequence += 1;

        session::save_message(
            &node,
            "session-1",
            sequence,
            "user",
            &serde_json::to_string(&tool_result).unwrap(),
        )
        .await
        .unwrap();
        sequence += 1;

        session::save_message(
            &node,
            "session-1",
            sequence,
            "assistant",
            &serde_json::to_string(&assistant).unwrap(),
        )
        .await
        .unwrap();
        sequence += 1;
    }

    let history = session::load_history(&node, "session-1").await.unwrap();
    let (stripped_history, _) = strip_tool_results(history);
    let result = compactor
        .compact(
            stripped_history,
            2000,
            &CompactionOptions {
                threshold: 0.50,
                keep_recent_tokens: 200,
                strategy: CompactionStrategy::Summarize,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let summary = result.summary.clone().unwrap();
    session::save_compaction_entry(
        &node,
        "session-1",
        &summary,
        &result.files_read,
        &result.files_modified,
        result.messages_compacted,
        result.original_token_estimate,
        result.compacted_token_estimate,
    )
    .await
    .unwrap();

    let entries = session::load_compaction_entries(&node, "session-1")
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].summary.contains("inspected the source files"));

    let resumed_history = session::load_history(&node, "session-1").await.unwrap();
    let (resumed_history, _) = strip_tool_results(resumed_history);
    let compacted_count = entries
        .iter()
        .map(|entry| entry.messages_compacted as usize)
        .sum::<usize>();
    let resumed_history = resumed_history
        .into_iter()
        .skip(compacted_count)
        .collect::<Vec<_>>();
    assert_eq!(resumed_history, result.messages);

    let prompt_builder = LayeredPromptBuilder::for_behavior(
        "Be helpful.",
        "general",
        &["list_files", "read_file", "bash"],
        true,
        crate::config::DEFAULT_CONTEXT_WINDOW,
        crate::config::DEFAULT_MAX_OUTPUT_TOKENS,
        &[],
    );
    let summaries = entries
        .iter()
        .map(|entry| entry.summary.clone())
        .collect::<Vec<_>>();
    let built = prompt_builder
        .build(&resumed_history, &summaries)
        .await
        .unwrap();

    if let Message::User { content } = &built.messages[0] {
        if let UserContent::Text(text) = content.first_ref() {
            assert!(text.text.contains("inspected the source files"));
            assert!(text
                .text
                .contains("Previous conversation summary (compacted):"));
        } else {
            panic!("expected summary reminder text");
        }
    } else {
        panic!("expected summary reminder");
    }

    assert_eq!(built.messages[1..], resumed_history[..]);

    let _ = std::fs::remove_dir_all(&data_path);
}
