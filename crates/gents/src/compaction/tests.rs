use std::sync::{Arc, Mutex};

use crate::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolResult, ToolResultContent, UserContent,
};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};

use super::*;
use crate::ensure_schemas;
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::session;
use crate::test_support::first_content;

fn text_msg(role: &str, text: &str) -> Message {
    match role {
        "user" => Message::User {
            content: vec![UserContent::Text(Text {
                text: text.to_string(),
            })],
        },
        "assistant" => Message::Assistant {
            id: None,
            content: vec![AssistantContent::Text(Text {
                text: text.to_string(),
            })],
        },
        _ => panic!("unknown role"),
    }
}

fn tool_call_msg(name: &str, args: &str) -> Message {
    Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "call-1".to_string(),
            call_id: Some("call-1".to_string()),
            function: crate::llm::message::ToolFunction {
                name: name.to_string(),
                arguments: serde_json::from_str(args).unwrap_or_default(),
            },
            signature: None,
            additional_params: None,
        })],
    }
}

fn tool_result_msg(call_id: &str, result_text: &str) -> Message {
    Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: call_id.to_string(),
            call_id: Some(call_id.to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: result_text.to_string(),
            })],
        })],
    }
}

fn tool_call_content(id: &str) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.to_string(),
        call_id: Some(id.to_string()),
        function: crate::llm::message::ToolFunction {
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
            content: vec![
                AssistantContent::Text(Text {
                    text: "thinking".to_string(),
                }),
                tool_call_content("call-A"),
                tool_call_content("call-B"),
            ],
        },
        tool_result_msg("call-A", "A-result"),
    ];

    let out = super::history::drop_unpaired_tool_calls(messages);

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
            content: vec![tool_call_content("call-X")],
        },
    ];
    let out = super::history::drop_unpaired_tool_calls(messages);
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
            content: vec![
                AssistantContent::Reasoning(crate::llm::message::Reasoning::new("why")),
                tool_call_content("call-A"),
                tool_call_content("call-B"),
                AssistantContent::Text(Text {
                    text: "answer".to_string(),
                }),
            ],
        },
        tool_result_msg("call-A", "A-result"),
    ];

    let out = super::history::normalize_assistant_content_order(messages);

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
            content: vec![
                AssistantContent::Text(Text {
                    text: "answer".to_string(),
                }),
                tool_call_content("call-A"),
            ],
        },
        tool_result_msg("call-A", "A-result"),
    ];
    let out = super::history::normalize_assistant_content_order(messages.clone());
    assert_eq!(out, messages);
}

#[test]
fn drop_unpaired_tool_calls_is_identity_when_all_paired() {
    let messages = vec![
        Message::Assistant {
            id: None,
            content: vec![tool_call_content("call-A")],
        },
        tool_result_msg("call-A", "A-result"),
        text_msg("user", "next"),
    ];
    let out = super::history::drop_unpaired_tool_calls(messages.clone());
    assert_eq!(
        out.len(),
        messages.len(),
        "fully-paired history must pass through unchanged"
    );
}

#[test]
fn drop_orphaned_tool_results_removes_results_without_preceding_calls() {
    // A compaction split (or compacted-prefix drop) can leave a tool result
    // whose assistant call was compacted away. Providers reject a tool message
    // with no preceding assistant tool call, so the orphan must be dropped;
    // paired results and other user content survive.
    let messages = vec![
        tool_result_msg("call-GONE", "orphaned"),
        text_msg("user", "continue"),
        Message::Assistant {
            id: None,
            content: vec![tool_call_content("call-A")],
        },
        tool_result_msg("call-A", "A-result"),
    ];

    let out = super::history::drop_orphaned_tool_results(messages);

    assert_eq!(
        out.len(),
        3,
        "the orphaned-result message is dropped entirely; got {out:?}"
    );
    assert!(
        matches!(&out[0], Message::User { content }
            if content.iter().any(|c| matches!(c, UserContent::Text(_)))),
        "the plain user message must lead after the orphan is dropped"
    );
    assert!(
        matches!(&out[2], Message::User { content }
            if content.iter().any(|c| matches!(c, UserContent::ToolResult(r)
                if r.call_id.as_deref() == Some("call-A")))),
        "the paired result must survive"
    );
}

#[test]
fn drop_orphaned_tool_results_keeps_mixed_user_content() {
    // A user message mixing text with an orphaned result keeps the text.
    let mixed = Message::User {
        content: vec![
            UserContent::Text(Text {
                text: "also this".to_string(),
            }),
            UserContent::ToolResult(ToolResult {
                id: "call-GONE".to_string(),
                call_id: Some("call-GONE".to_string()),
                content: vec![ToolResultContent::Text(Text {
                    text: "orphaned".to_string(),
                })],
            }),
        ],
    };
    let out = super::history::drop_orphaned_tool_results(vec![mixed]);
    assert_eq!(out.len(), 1);
    let Message::User { content } = &out[0] else {
        panic!("expected user message");
    };
    assert_eq!(content.len(), 1);
    assert!(matches!(content.first(), Some(UserContent::Text(_))));
}

#[test]
fn drop_orphaned_tool_results_removes_results_after_conversation_resumes() {
    // OpenAI chat-completions accepts tool results only while they are closing
    // the active assistant tool-call turn. A matching call somewhere earlier in
    // the transcript is not enough once normal conversation has resumed.
    let messages = vec![
        Message::Assistant {
            id: None,
            content: vec![tool_call_content("call-A")],
        },
        text_msg("assistant", "I moved on without the tool result."),
        tool_result_msg("call-A", "late result"),
    ];

    let out = super::history::drop_orphaned_tool_results(messages);

    assert_eq!(
        out.len(),
        2,
        "late tool result must be dropped after assistant conversation resumes; got {out:?}"
    );
    assert!(
        !out.iter().any(|message| matches!(message,
            Message::User { content }
                if content.iter().any(|item| matches!(item, UserContent::ToolResult(_))))),
        "no tool result should survive after the active tool-call turn closed"
    );
}

#[test]
fn sanitize_history_for_provider_drops_stale_result_and_now_unpaired_call() {
    let messages = vec![
        Message::Assistant {
            id: None,
            content: vec![tool_call_content("call-A")],
        },
        text_msg("assistant", "No tool result arrived."),
        tool_result_msg("call-A", "late result"),
    ];

    let out = super::sanitize_history_for_provider(messages);
    assert_eq!(
        out,
        vec![text_msg("assistant", "No tool result arrived.")],
        "the stale result is orphaned, then the now-unpaired tool call is dropped"
    );
}

#[test]
fn sanitize_history_for_provider_drops_orphans_in_both_directions() {
    // Unpaired call AND orphaned result in one history: both removed, the
    // paired exchange survives.
    let messages = vec![
        tool_result_msg("call-GONE", "orphaned"),
        Message::Assistant {
            id: None,
            content: vec![tool_call_content("call-UNPAIRED")],
        },
        Message::Assistant {
            id: None,
            content: vec![tool_call_content("call-A")],
        },
        tool_result_msg("call-A", "A-result"),
    ];
    let out = super::sanitize_history_for_provider(messages);
    assert_eq!(
        out.len(),
        2,
        "only the paired exchange survives; got {out:?}"
    );
}

#[test]
fn sanitize_repairs_result_preceding_its_call() {
    // P1 counterexample for the unpaired-first composition (found while
    // proof-sketching the PromptAssembly Lean model): a result that PRECEDES
    // its call (backfill ordering, P2P-merged transcripts). The result must be
    // dropped as orphaned AND the call must then be dropped as unpaired —
    // orphan-drop must run first, or the call survives on the strength of a
    // result that no longer exists and an unpaired call reaches the provider.
    let messages = vec![
        tool_result_msg("call-A", "early result"),
        Message::Assistant {
            id: None,
            content: vec![tool_call_content("call-A")],
        },
    ];
    let out = super::sanitize_history_for_provider(messages);
    assert!(
        out.is_empty(),
        "result-before-call must sanitize to empty (orphan and unpaired both dropped); got {out:?}"
    );
}

#[test]
fn bounded_summary_truncates_oversized_model_emitted_summaries() {
    // The compaction summary is model-emitted free text injected into every
    // later request's system reminder — it must be bounded on its way to the
    // prompt (covers oversized entries already persisted, too).
    let oversized = "s".repeat(200 * 1024);
    let bounded = super::bounded_summary(oversized.clone());
    assert!(
        bounded.len() < oversized.len(),
        "oversized summary must be bounded"
    );
    assert!(!bounded.is_empty());

    let small = "concise summary".to_string();
    assert_eq!(
        super::bounded_summary(small.clone()),
        small,
        "small summaries pass through untouched"
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
            choice: rig::one_or_many::OneOrMany::one(rig::completion::AssistantContent::Text(
                rig::completion::message::Text {
                    text: self.response.clone(),
                },
            )),
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

/// Counts provider calls and always fails transiently — to prove compaction
/// does not retry. `Clone` shares the counter (the loop clones the model).
#[derive(Clone)]
struct CountingFailModel {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl CountingFailModel {
    fn new() -> Self {
        Self {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        self.calls.clone()
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for CountingFailModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self::new()
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(CompletionError::ProviderError(
            "transient compaction failure".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(CompletionError::ProviderError(
            "transient compaction failure".to_string(),
        ))
    }
}

#[tokio::test]
async fn compaction_fails_fast_without_retrying_on_transient_error() {
    // #648: compaction is an internal sub-completion, not a user execution
    // origin. A transient provider failure must fail fast, NOT inherit the
    // scheduled retry ladder (5s/30s/120s, deadline-less) that would block
    // inline compaction for minutes. `DefraCompactor::new` forces `no_retry`
    // even when handed a `scheduled_default` config, so exactly one provider
    // call is made and `compact` returns promptly with the error.
    let model = CountingFailModel::new();
    let calls = model.calls();
    let config = crate::agent::loop_stream::LoopConfig {
        preamble: None,
        context_message: None,
        temperature: None,
        max_tokens: None,
        additional_params: None,
        tool_choice: None,
        on_rendered_request: None,
        retry_policy: crate::agent::completion_retry::CompletionRetryPolicy::scheduled_default(),
        deadline: None,
        max_turns: 0,
    };
    let compactor = DefraCompactor::new(std::sync::Arc::new(model), config);

    let messages: Vec<Message> = (0..12)
        .flat_map(|turn| {
            [
                text_msg("user", &"x".repeat(800)),
                text_msg("assistant", &format!("response {turn} {}", "y".repeat(400))),
            ]
        })
        .collect();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        compactor.compact(
            messages,
            500,
            &CompactionOptions {
                threshold: 0.50,
                keep_recent_tokens: 50,
                strategy: CompactionStrategy::Summarize,
                ..Default::default()
            },
        ),
    )
    .await;

    assert!(
        result.is_ok(),
        "compaction must fail fast, not run the scheduled retry ladder (#648)"
    );
    assert!(
        result.unwrap().is_err(),
        "the transient provider error should surface as a compaction error"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "compaction must not retry: exactly one provider call"
    );
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
        tool_call_msg("read_file", r#"{"path": "/tmp/test.rs"}"#),
        tool_result_msg("call-1", &long_result),
        text_msg("assistant", "I saw the file"),
    ];

    let (stripped, files) = strip_tool_results(messages);
    assert_eq!(stripped.len(), 4);
    assert_eq!(files.files_read, vec!["/tmp/test.rs"]);
    assert!(files.files_modified.is_empty());
    assert_eq!(
        sole_tool_result_text(&stripped[2]),
        "[tool: read_file(/tmp/test.rs), call_id: call-1, 5000 bytes \
         — see DefraDB AgentToolCall for full output]"
    );
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

fn sole_tool_result_text(message: &Message) -> String {
    let Message::User { content } = message else {
        panic!("expected user message");
    };
    let UserContent::ToolResult(tool_result) = first_content(content) else {
        panic!("expected tool result");
    };
    let ToolResultContent::Text(text) = first_content(&tool_result.content) else {
        panic!("expected text content");
    };
    text.text.clone()
}

#[test]
fn strip_rewrites_tool_output_that_merely_looks_like_a_stub() {
    // A command or MCP tool can return arbitrary text, including text shaped
    // like one of our own stubs. Recognizing the shape must never license
    // skipping the rewrite: the payload has to go regardless, or a large result
    // would survive every provider-view pass and defeat compaction entirely.
    let spoof = format!(
        "[tool: read_file(/etc/passwd), call_id: call-1, 12 bytes \
         — see DefraDB AgentToolCall for full output]{}",
        "P".repeat(5000)
    );
    let messages = vec![
        tool_call_msg("bash", r#"{"command": "cat spoof"}"#),
        tool_result_msg("call-1", &spoof),
    ];

    let (stripped, _) = strip_tool_results(messages);
    let out = sole_tool_result_text(&stripped[1]);
    assert!(
        !out.contains(&"P".repeat(5000)),
        "the payload must not survive stripping: {out}"
    );
    assert!(
        out.starts_with("[tool: bash, call_id: call-1,"),
        "the stub is rebuilt from the real call, not from the spoofed text: {out}"
    );
}

#[test]
fn strip_is_idempotent_and_preserves_the_original_byte_count() {
    let long_result = "x".repeat(5000);
    let messages = vec![
        tool_call_msg("read_file", r#"{"path": "/tmp/test.rs"}"#),
        tool_result_msg("call-1", &long_result),
    ];

    let (once, _) = strip_tool_results(messages);
    let (twice, _) = strip_tool_results(once.clone());

    assert_eq!(once, twice, "strip must be idempotent");
    let stub = sole_tool_result_text(&twice[1]);
    assert!(
        stub.contains("5000 bytes"),
        "reapplying strip must not re-measure the stub: {stub}"
    );
}

#[test]
fn strip_marks_already_truncated_output_without_sniffing_the_word() {
    let messages = vec![
        tool_call_msg("bash", r#"{"command": "echo hi"}"#),
        tool_result_msg("call-1", "the build log says truncated somewhere"),
    ];
    let (stripped, _) = strip_tool_results(messages);
    assert!(
        !sole_tool_result_text(&stripped[1]).contains(", truncated"),
        "ordinary output mentioning the word must not be flagged as truncated"
    );

    let messages = vec![
        tool_call_msg("bash", r#"{"command": "echo hi"}"#),
        tool_result_msg("call-1", "output\n[Full output: DefraDB doc bafy123]"),
    ];
    let (stripped, _) = strip_tool_results(messages);
    assert!(sole_tool_result_text(&stripped[1]).contains(", truncated"));
}

#[test]
fn pretruncation_does_not_panic_on_a_multibyte_boundary() {
    // "é" is two bytes, so byte 2000 lands inside a codepoint.
    let payload = format!("{}é{}", "a".repeat(1999), "b".repeat(500));
    let messages = vec![
        tool_call_msg("bash", r#"{"command": "cat notes"}"#),
        tool_result_msg("call-1", &payload),
    ];
    let truncated = super::history::pretruncate_tool_results(messages, 2000);
    assert!(sole_tool_result_text(&truncated[1]).contains("pre-truncated"));
}

#[test]
fn file_activity_classifies_the_registered_file_tools() {
    let messages = vec![
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
        tool_call_msg("write_file", r#"{"path": "/src/lib.rs"}"#),
        tool_result_msg("call-1", "ok"),
        tool_call_msg("edit_file", r#"{"path": "/src/edit.rs"}"#),
        tool_result_msg("call-1", "ok"),
        tool_call_msg("grep", r#"{"path": "/src/grep.rs"}"#),
        tool_result_msg("call-1", "hit"),
        tool_call_msg("glob", r#"{"path": "/src/glob.rs"}"#),
        tool_result_msg("call-1", "hit"),
        tool_call_msg("list_files", r#"{"path": "/src/list.rs"}"#),
        tool_result_msg("call-1", "hit"),
    ];

    let (_, files) = strip_tool_results(messages);
    assert_eq!(
        files.files_read,
        vec![
            "/src/glob.rs",
            "/src/grep.rs",
            "/src/list.rs",
            "/src/main.rs"
        ]
    );
    assert_eq!(files.files_modified, vec!["/src/edit.rs", "/src/lib.rs"]);
}

#[test]
fn dry_run_edits_are_not_recorded_as_modifications() {
    let messages = vec![
        tool_call_msg(
            "edit_file",
            r#"{"path": "/src/preview.rs", "dry_run": true}"#,
        ),
        tool_result_msg("call-1", "would change 3 lines"),
    ];

    let (_, files) = strip_tool_results(messages);
    assert!(
        files.files_modified.is_empty(),
        "a dry run writes nothing: {:?}",
        files.files_modified
    );
    assert_eq!(
        files.files_read,
        vec!["/src/preview.rs"],
        "it did read the file to build the preview"
    );
}

#[test]
fn calls_without_a_result_are_not_recorded_as_modifications() {
    // The turn was interrupted before the write ran: an assistant announcement
    // with no paired result must not be persisted under "Files modified", where
    // it would be rendered into later prompts as state the run never produced.
    let messages = vec![
        tool_call_msg("write_file", r#"{"path": "/src/never_written.rs"}"#),
        text_msg("user", "actually, stop"),
    ];

    let (_, files) = strip_tool_results(messages);
    assert!(
        files.files_modified.is_empty(),
        "unpaired call must not count: {:?}",
        files.files_modified
    );

    // The same history *with* a result does count.
    let completed = vec![
        tool_call_msg("write_file", r#"{"path": "/src/written.rs"}"#),
        tool_result_msg("call-1", "ok"),
    ];
    let (_, files) = strip_tool_results(completed);
    assert_eq!(files.files_modified, vec!["/src/written.rs"]);
}

#[test]
fn every_registered_file_tool_is_classified() {
    // Guards against a file tool being added to toolset::file_tools without a
    // matching classification here, which would silently empty the compaction
    // summary's file lists — the defect this test exists to keep from recurring.
    for name in ["read_file", "list_files", "glob", "grep"] {
        assert!(
            super::history::is_read_tool(name),
            "{name} is not classified as a read tool"
        );
    }
    for name in ["write_file", "edit_file"] {
        assert!(
            super::history::is_write_tool(name),
            "{name} is not classified as a write tool"
        );
    }
}

#[test]
fn split_never_separates_a_tool_call_from_its_result() {
    // A budget that retains roughly the last message would land the boundary
    // between the assistant tool call and the user tool result.
    let messages = vec![
        text_msg("user", &"a".repeat(4000)),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];

    let (old, recent) = super::history::split_messages_for_summary(messages, 40);

    assert_eq!(
        old.len(),
        1,
        "only the bulky user turn should be summarized"
    );
    assert_eq!(
        recent.len(),
        2,
        "the assistant turn and its result stay together"
    );
    assert!(
        matches!(&recent[0], Message::Assistant { .. }),
        "the retained tail must start at the assistant announcement"
    );
}

#[test]
fn pair_safe_boundary_retreats_to_the_turn_start() {
    let messages = vec![
        text_msg("user", "go"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];

    assert_eq!(super::history::pair_safe_boundary(&messages, 2), 1);
    assert_eq!(super::history::pair_safe_boundary(&messages, 3), 3);
    assert_eq!(super::history::pair_safe_boundary(&messages, 1), 1);
}

#[test]
fn provider_view_is_idempotent() {
    let history = vec![
        tool_result_msg("orphan-1", "result with no call"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];
    let (once, _) = provider_view(history);
    let (twice, _) = provider_view(once.clone());
    assert_eq!(once, twice);
}

#[test]
fn compacted_prefix_is_counted_and_dropped_in_the_same_space() {
    // An orphaned tool result at the head: sanitize removes it, so the
    // unsanitized and sanitized indexings of the compacted prefix diverge.
    // Under the old order (strip -> drop -> sanitize) a count measured in the
    // sanitized space was applied to the unsanitized one, shifting the boundary.
    let history = vec![
        tool_result_msg("orphan-1", "result with no call"),
        text_msg("user", "first real turn"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
        text_msg("assistant", "done"),
        text_msg("user", "second turn"),
    ];

    let (view, _) = provider_view(history.clone());
    assert_eq!(
        view.len(),
        5,
        "sanitize must remove the orphaned result from the view"
    );

    // Compaction summarized the first row *of the view* — a pair-safe boundary,
    // which is the only kind the writer ever records.
    let compacted = 1usize;
    assert_eq!(
        super::history::pair_safe_boundary(&view, compacted),
        compacted,
        "the modelled writer only ever records a pair-safe boundary"
    );
    let retained = view.iter().skip(compacted).cloned().collect::<Vec<_>>();

    // The next request rebuilds the view from the same durable history and
    // drops the same count. It must land on exactly the retained rows.
    let (reread, _) = provider_view(history.clone());
    assert_eq!(
        reread.into_iter().skip(compacted).collect::<Vec<_>>(),
        retained
    );

    // The old order is the defect: the count was measured against the sanitized
    // list but applied to the unsanitized one, which still carries the orphan at
    // index 0. Dropping one row there removes the orphan instead of the
    // summarized turn, so "first real turn" survives verbatim alongside its own
    // summary.
    let (stripped, _) = strip_tool_results(history);
    let old_order =
        sanitize_history_for_provider(stripped.into_iter().skip(compacted).collect::<Vec<_>>());
    assert_eq!(
        old_order.len(),
        retained.len() + 1,
        "the old order retains one row too many"
    );
    assert_eq!(
        old_order.first(),
        Some(&text_msg("user", "first real turn")),
        "and the row it retains is the one that was summarized"
    );
}

#[test]
fn safe_to_reduce_requires_every_retained_tool_result_to_be_terminal() {
    let messages = vec![
        text_msg("user", "go"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];

    assert!(safe_to_reduce(&messages, &AllTerminal));
    assert!(!safe_to_reduce(&messages, &NoneKnown));

    // No tool results at all: nothing to gate on.
    let plain = vec![text_msg("user", "go"), text_msg("assistant", "ok")];
    assert!(safe_to_reduce(&plain, &NoneKnown));
}

struct StreamingIndex;

impl ResponseStatusIndex for StreamingIndex {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        Some(ResponseStatus::Streaming)
    }
}

#[test]
fn safe_to_reduce_is_closed_while_a_response_is_streaming() {
    let messages = vec![
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];
    assert!(!safe_to_reduce(&messages, &StreamingIndex));
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
    let data_path = std::env::temp_dir().join(format!("gents-compactor-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();
    session::create_session_with_id(&node, "session-1", "general", "did:test:test")
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
        context_message: None,
        temperature: None,
        max_tokens: None,
        additional_params: None,
        tool_choice: None,
        on_rendered_request: None,
        retry_policy: crate::agent::completion_retry::CompletionRetryPolicy::scheduled_default(),
        deadline: None,
        max_turns: 0,
    };
    let compactor = DefraCompactor::new(std::sync::Arc::new(model), config);

    let mut sequence = 1;
    for turn in 0..55 {
        let user = Message::User {
            content: vec![UserContent::Text(Text {
                text: format!("Request {turn}: {}", "x".repeat(800)),
            })],
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
            "did:test:test",
            sequence,
            "user",
            &serde_json::to_string(&user).unwrap(),
            None,
        )
        .await
        .unwrap();
        sequence += 1;

        session::save_message(
            &node,
            "session-1",
            "did:test:test",
            sequence,
            "assistant",
            &serde_json::to_string(&assistant_tool_call).unwrap(),
            None,
        )
        .await
        .unwrap();
        sequence += 1;

        session::save_message(
            &node,
            "session-1",
            "did:test:test",
            sequence,
            "user",
            &serde_json::to_string(&tool_result).unwrap(),
            None,
        )
        .await
        .unwrap();
        sequence += 1;

        session::save_message(
            &node,
            "session-1",
            "did:test:test",
            sequence,
            "assistant",
            &serde_json::to_string(&assistant).unwrap(),
            None,
        )
        .await
        .unwrap();
        sequence += 1;
    }

    let history = session::load_history(&node, "session-1").await.unwrap();
    let durable_before = history.clone();
    let (provider_history, _) = provider_view(history);
    let result = compactor
        .compact(
            provider_history,
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
        "did:test:test",
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
    // Compaction is a projection: it writes a summary entry and drops a prefix
    // from the *provider view*. The durable AgentMessage rows that
    // `run_timeline` reconstructs a request's event stream from must be
    // untouched.
    assert_eq!(
        durable_before, resumed_history,
        "compaction must not mutate the durable transcript the timeline is built from"
    );

    // Read side: rebuild the same provider view and drop the same count. This
    // is the write/read correspondence — the count was measured against
    // `provider_view` above, so it must be applied to `provider_view` here.
    let (resumed_history, _) = provider_view(resumed_history);
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
        if let UserContent::Text(text) = first_content(content) {
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
