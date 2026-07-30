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

#[test]
fn t1_sanitize_is_sound_on_dirty_input() {
    let dirty = vec![
        result("call-gone"),
        user("hello"),
        assistant_calls(&["call-a", "call-unpaired"]),
        result("call-a"),
        assistant_calls(&["call-dangling"]),
    ];
    let out = sanitize_history_for_provider(dirty);
    assert_provider_valid(&out);
    assert_eq!(
        out.len(),
        3,
        "user + paired call + its result survive: {out:?}"
    );
}

#[test]
fn t1_composition_order_result_before_call_sanitizes_to_empty() {
    let out = sanitize_history_for_provider(vec![result("call-a"), assistant_calls(&["call-a"])]);
    assert!(out.is_empty(), "expected empty, got {out:?}");
}

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
    for split in 0..=transcript.len() {
        let recent = transcript[split..].to_vec();
        let out = sanitize_history_for_provider(recent);
        assert_provider_valid(&out);
    }
}

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

use gents::llm::tool::normalize_tool_call_arguments;

const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
     <function=describe_tool>\", \"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";

fn normalize_args(value: &serde_json::Value) -> serde_json::Value {
    normalize_tool_call_arguments("conformance", "echo", value)
}

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
