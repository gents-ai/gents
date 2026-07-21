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

use gents::compaction::sanitize_history_for_provider;
use gents::llm::message::{
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
                        // Pairing identity must mirror history.rs::tool_call_key:
                        // `call_id` when present (OpenAI responses-API shape),
                        // falling back to the item id.
                        pending.push(
                            tool_call
                                .call_id
                                .clone()
                                .unwrap_or_else(|| tool_call.id.clone()),
                        );
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

/// Pairing identity: calls and results pair on `call_id.unwrap_or(id)`
/// (`history.rs::tool_call_key` / `tool_result_key`), never on the item id
/// alone. The Lean model abstracts both into one `ToolCallId`, so this
/// below-the-model identity is fenced only here: a pair whose `call_id`
/// differs from both item ids (the OpenAI responses-API shape) must survive
/// sanitize and validate.
#[test]
fn pairing_identity_uses_call_id_over_item_id() {
    let history = vec![
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "item-9".to_string(),
                call_id: Some("fc-1".to_string()),
                function: ToolFunction {
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                signature: None,
                additional_params: None,
            })],
        },
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "out-3".to_string(),
                call_id: Some("fc-1".to_string()),
                content: vec![ToolResultContent::Text(Text {
                    text: "fc-1-result".to_string(),
                })],
            })],
        },
    ];
    let out = sanitize_history_for_provider(history.clone());
    assert_eq!(out, history);
    assert_provider_valid(&out);
}

// ===== ToolArgs (issues #589/#590): value-granular argument-shape contract =====
//
// Mirrors `Proofs/PromptAssembly/ToolArgs.lean` against the Rust normalizer
// `llm::tool::normalize_tool_call_arguments` (applied at both rig-converter
// seams). The Lean model is below row granularity — normalization is pointwise
// per tool call and never changes row shape — so these vectors fence the value
// contract the row-granular sanitize theorems cannot see.

use gents::llm::tool::normalize_tool_call_arguments;

/// The #589 production poison (Amy's persisted row `Rrt-HmhWfFSmkh1HSUmHt`):
/// out-of-channel contamination with LITERAL newlines inside strings,
/// duplicated keys, and the intended call surviving as the final `tool_name`.
const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
     <function=describe_tool>\", \"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";

fn normalize_args(value: &serde_json::Value) -> serde_json::Value {
    normalize_tool_call_arguments("conformance", "echo", value)
}

/// All non-object shapes a `serde_json::Value` can carry into the seam:
/// the #590 reproduction matrix (string-encoded and native forms) plus the
/// #589 corrupt raw string.
fn nonobject_vectors() -> Vec<serde_json::Value> {
    vec![
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!("[]"),
        serde_json::json!("[1,2]"),
        serde_json::json!("123"),
        serde_json::json!("true"),
        serde_json::json!("null"),
        serde_json::json!("\"any string\""),
        serde_json::json!("not json at all"),
        serde_json::json!([]),
        serde_json::json!([1, 2]),
        serde_json::json!(123),
        serde_json::json!(true),
        serde_json::Value::String(CORRUPT_TOOL_ARGS_589.to_string()),
        serde_json::json!("{\"city\":\"NYC\"}"),
        serde_json::json!({"city": "NYC"}),
    ]
}

/// Lean `normalize_isObject` (N1, soundness): normalization always yields an
/// object, whatever shape came in — no egress path can hand the provider a
/// non-object `arguments` value.
#[test]
fn tool_args_n1_normalize_always_yields_object() {
    for vector in nonobject_vectors() {
        let normalized = normalize_args(&vector);
        assert!(
            normalized.is_object(),
            "N1 violated: {vector:?} normalized to non-object {normalized:?}"
        );
    }
}

/// Lean `normalize_fixpoint_of_isObject` (N2, object fixpoint): a well-formed
/// object passes through byte-identical — the healthy flow has no regression.
#[test]
fn tool_args_n2_object_passes_through_unchanged() {
    let objects = [
        serde_json::json!({}),
        serde_json::json!({"city": "NYC"}),
        serde_json::json!({"nested": {"deep": [1, 2, {"x": null}]}}),
    ];
    for object in objects {
        assert_eq!(
            normalize_args(&object),
            object,
            "N2 violated: object arguments must be unchanged"
        );
    }
}

/// Lean `normalize_idempotent` (N3): the ingest and egress seams compose —
/// a value persisted normalized re-egresses identical.
#[test]
fn tool_args_n3_normalize_is_idempotent() {
    for vector in nonobject_vectors() {
        let once = normalize_args(&vector);
        assert_eq!(
            normalize_args(&once),
            once,
            "N3 violated: double normalization drifted for {vector:?}"
        );
    }
}

/// Lean `normalize_salvages_str` (N4, salvage): a string that (post-repair)
/// parses to an object recovers THAT object — the intended call survives
/// rather than collapsing to the empty fallback. Includes the #589 corrupt
/// production payload.
#[test]
fn tool_args_n4_stringified_object_recovers_its_payload() {
    assert_eq!(
        normalize_args(&serde_json::json!("{\"city\":\"NYC\"}")),
        serde_json::json!({"city": "NYC"})
    );

    let salvaged = normalize_args(&serde_json::Value::String(
        CORRUPT_TOOL_ARGS_589.to_string(),
    ));
    assert!(salvaged.is_object(), "N4: the #589 payload must salvage");
    assert_eq!(
        salvaged["tool_name"], "list_hosts",
        "N4: the intended call must survive the salvage"
    );
}

/// Lean `normalize_nonobject_to_empty`: the non-salvageable shapes collapse to
/// exactly the EMPTY object, pinning the entire coercion table.
#[test]
fn tool_args_nonobject_collapses_to_empty_object() {
    for vector in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!("[]"),
        serde_json::json!("123"),
        serde_json::json!("\"any string\""),
        serde_json::json!("not json at all"),
        serde_json::json!([]),
        serde_json::json!(123),
        serde_json::json!(true),
    ] {
        assert_eq!(
            normalize_args(&vector),
            serde_json::json!({}),
            "non-salvageable {vector:?} must collapse to {{}}"
        );
    }
}
