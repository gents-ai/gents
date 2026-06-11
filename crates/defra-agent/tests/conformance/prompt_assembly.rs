//! PromptAssembly conformance (issue #448).
//!
//! Mirrors the Lean PromptAssembly provider-input boundary against the Rust
//! implementation in `compaction::sanitize_history_for_provider`. Each test
//! names the theorem it fences; the vectors are the same row-granular shapes
//! the Lean `Executable` definitions compute over, with the Rust assertions
//! additionally enforcing the stricter provider shape that tool results must
//! close the active assistant tool-call block before normal conversation
//! resumes.
//!
//! Content-order normalization (`normalize_assistant_content_order`) is part
//! of the Rust sanitizer but NOT yet part of the Lean model (deferred to the
//! `MessageKind` content-order extension); all vectors here use
//! canonical-content messages so the full Rust composition is exercised while
//! staying within the modeled fragment.

use defra_agent::compaction::sanitize_history_for_provider;
use defra_agent::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};

fn call(id: &str) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.to_string(),
        call_id: Some(id.to_string()),
        function: ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    })
}

fn assistant_calls(ids: &[&str]) -> Message {
    Message::Assistant {
        id: None,
        content: ids.iter().map(|id| call(id)).collect::<Vec<_>>(),
    }
}

fn result(id: &str) -> Message {
    Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: id.to_string(),
            call_id: Some(id.to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: format!("{id}-result"),
            })],
        })],
    }
}

fn user(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

/// Provider-valid history at row granularity: every assistant tool-call row
/// opens a pending result block, only matching tool results may appear while
/// that block is open, and ordinary conversation may resume only after the
/// pending block is closed.
fn assert_provider_valid(msgs: &[Message]) {
    let mut pending: Vec<String> = Vec::new();
    for message in msgs {
        match message {
            Message::Assistant { content, .. } => {
                assert!(
                    pending.is_empty(),
                    "assistant turn arrived before prior tool calls were resolved: {pending:?}"
                );
                for item in content.iter() {
                    if let AssistantContent::ToolCall(tool_call) = item {
                        pending.push(tool_call.id.clone());
                    }
                }
            }
            Message::User { content } => {
                let has_tool_results = content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_)));
                if !has_tool_results {
                    assert!(
                        pending.is_empty(),
                        "ordinary user content arrived before tool calls were resolved: {pending:?}"
                    );
                    continue;
                }
                for item in content.iter() {
                    if let UserContent::ToolResult(tool_result) = item {
                        let key = tool_result
                            .call_id
                            .clone()
                            .unwrap_or_else(|| tool_result.id.clone());
                        let position = pending.iter().position(|call| call == &key);
                        assert!(
                            position.is_some(),
                            "tool result '{key}' is not closing the active tool-call turn"
                        );
                        pending.remove(position.unwrap());
                    }
                }
            }
            Message::System { .. } => {
                assert!(
                    pending.is_empty(),
                    "system content arrived before tool calls were resolved: {pending:?}"
                );
            }
        }
    }
    assert!(
        pending.is_empty(),
        "provider history ended with unpaired calls {pending:?}"
    );
}

/// T1 (`sanitize_sound`): arbitrary dirty input sanitizes to ProviderValid.
#[test]
fn t1_sanitize_is_sound_on_dirty_input() {
    let dirty = vec![
        result("call-gone"), // orphaned result
        user("hello"),
        assistant_calls(&["call-a", "call-unpaired"]),
        result("call-a"),
        assistant_calls(&["call-dangling"]), // unpaired, whole turn drops
    ];
    let out = sanitize_history_for_provider(dirty);
    assert_provider_valid(&out);
    assert_eq!(
        out.len(),
        3,
        "user + paired call + its result survive: {out:?}"
    );
}

/// T1 ordering direction: the composition runs orphan-drop FIRST. The Lean
/// counterexample `[result A, call A]` must sanitize to [] — with the
/// swapped composition the call survives on the strength of the result it
/// then drops, and an unpaired call reaches the provider.
#[test]
fn t1_composition_order_result_before_call_sanitizes_to_empty() {
    let out = sanitize_history_for_provider(vec![result("call-a"), assistant_calls(&["call-a"])]);
    assert!(out.is_empty(), "expected empty, got {out:?}");
}

/// T1 active-turn direction: a result that matches an earlier call is still
/// stale if normal conversation resumed first. The result must drop, and then
/// the now-unpaired assistant call drops too.
#[test]
fn t1_result_after_conversation_resumes_sanitizes_to_plain_history() {
    let out = sanitize_history_for_provider(vec![
        assistant_calls(&["call-a"]),
        user("already moved on"),
        result("call-a"),
    ]);
    assert_eq!(out, vec![user("already moved on")]);
    assert_provider_valid(&out);
}

/// T2 (`sanitize_fixpoint`): provider-valid history passes through
/// unchanged — nothing valid is dropped or reordered.
#[test]
fn t2_sanitize_is_identity_on_valid_history() {
    let valid = vec![
        user("question"),
        assistant_calls(&["call-a", "call-b"]),
        result("call-a"),
        result("call-b"),
        user("follow-up"),
    ];
    let out = sanitize_history_for_provider(valid.clone());
    assert_eq!(out, valid);
}

/// T3 (`sanitize_idempotent`): one pass through the boundary is enough.
#[test]
fn t3_sanitize_is_idempotent() {
    let dirty = vec![
        result("call-gone"),
        assistant_calls(&["call-a", "call-unpaired"]),
        result("call-a"),
    ];
    let once = sanitize_history_for_provider(dirty);
    let twice = sanitize_history_for_provider(once.clone());
    assert_eq!(twice, once);
}

/// T4 (`sanitize_split_stable`): a pair-blind split's recent window — here
/// cutting between a call and its result, and mid multi-call turn —
/// sanitizes to ProviderValid. Instance of T1 over the suffix.
#[test]
fn t4_pair_blind_split_windows_sanitize_clean() {
    let transcript = vec![
        user("start"),
        assistant_calls(&["call-a"]),
        result("call-a"),
        assistant_calls(&["call-b", "call-c"]),
        result("call-b"),
        result("call-c"),
        user("end"),
    ];
    // Every possible split point: the recent window must always sanitize
    // to provider-valid history.
    for split in 0..=transcript.len() {
        let recent = transcript[split..].to_vec();
        let out = sanitize_history_for_provider(recent);
        assert_provider_valid(&out);
    }
}

/// T5 (`threaded_turn_fixpoint`): the owned loop's threaded turn shape —
/// one assistant tool-call turn followed by its results — is a fixpoint.
/// This is the justification for the `run_loop_stream` entry chokepoint
/// sanitizing only the LOADED history, never the loop's in-flight messages.
#[test]
fn t5_loop_threaded_turn_is_a_fixpoint() {
    let threaded = vec![
        assistant_calls(&["call-1", "call-2"]),
        result("call-1"),
        result("call-2"),
    ];
    let out = sanitize_history_for_provider(threaded.clone());
    assert_eq!(out, threaded);
}
