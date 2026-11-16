use gents_protocol::message::{AssistantContent, Message, ToolResultContent, UserContent};

use crate::loop_stream::TaggedMessage;

#[test]
fn assembles_context_immediately_before_prompt() {
    let context = Message::user("<context>\nseat: x\n</context>");
    let prompt = Message::user("hello");

    let with_context = super::assemble_new_messages(
        Some(context.clone()),
        TaggedMessage::unassociated(prompt.clone()),
    );
    assert_eq!(with_context.len(), 2);
    assert!(super::is_request_context_message(&with_context[0].message));
    assert_eq!(with_context[1].message, prompt);
    assert!(with_context.iter().all(|row| row.source.is_none()));
    assert_eq!(&with_context[with_context.len() - 2].message, &context);

    let without = super::assemble_new_messages(None, TaggedMessage::unassociated(prompt.clone()));
    assert_eq!(without, vec![TaggedMessage::unassociated(prompt)]);
}

#[test]
fn tagged_provider_view_uses_emitted_source_index_for_equal_assistant_rows() {
    use crate::claude_messages_body::ReplayTag;
    use gents_protocol::output::OutputSource;
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let tag = |seq| ReplayTag {
        request_doc_id: "request-doc".to_string(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq,
            },
            turn_index: seq as u32,
            attempt: 0,
        },
    };
    let first = tag(1);
    let second = tag(2);
    let identical = Message::assistant("same native bytes");
    let projected = super::tagged_history::provider_view_tagged(vec![
        TaggedMessage {
            message: identical.clone(),
            source: Some(first.clone()),
            physical_header: None,
            block_indices: Vec::new(),
        },
        TaggedMessage::unassociated(Message::user("between")),
        TaggedMessage {
            message: identical,
            source: Some(second.clone()),
            physical_header: None,
            block_indices: Vec::new(),
        },
    ])
    .expect("source-indexed projection");
    assert_eq!(projected[0].source.as_ref(), Some(&first));
    assert_eq!(projected[2].source.as_ref(), Some(&second));
    assert_eq!(projected[0].message, projected[2].message);
}

#[test]
fn is_request_context_message_only_matches_context_user_text() {
    assert!(super::is_request_context_message(&Message::user(
        "<context>\nx\n</context>"
    )));
    assert!(!super::is_request_context_message(&Message::user(
        "an ordinary prompt"
    )));
    assert!(!super::is_request_context_message(&Message::assistant(
        "hi"
    )));
}

#[test]
fn repair_preserves_a_tool_pair_split_across_history_and_prompt() {
    let history = vec![Message::Assistant {
        request_id: None,
        id: None,
        content: vec![AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                raw_arguments: None,
                id: "call-1".to_string(),
                call_id: Some("call-1".to_string()),
                function: gents_protocol::message::ToolFunction {
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "bad\nvalue" }),
                },
                signature: None,
                additional_params: None,
            },
        )],
    }];
    let new_messages = vec![Message::User {
        content: vec![UserContent::ToolResult(
            gents_protocol::message::ToolResult {
                id: "call-1".to_string(),
                call_id: Some("call-1".to_string()),
                content: vec![ToolResultContent::Text(gents_protocol::message::Text {
                    text: "done".to_string(),
                })],
            },
        )],
    }];
    let mut history = history
        .into_iter()
        .map(TaggedMessage::unassociated)
        .collect();
    let mut new_messages = new_messages
        .into_iter()
        .map(TaggedMessage::unassociated)
        .collect();

    super::repair::repair_provider_input(&mut history, &mut new_messages).unwrap();

    assert!(matches!(
        history.as_slice(),
        [TaggedMessage {
            message: Message::Assistant { .. },
            ..
        }]
    ));
    assert!(matches!(
        new_messages.as_slice(),
        [TaggedMessage {
            message: Message::User { .. },
            ..
        }]
    ));
    let joined = history
        .iter()
        .chain(new_messages.iter())
        .map(|row| row.message.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        crate::compaction::sanitize_history_for_provider(
            crate::provider_input::ProviderInputProfile::OpenAiChatCompletions,
            joined.clone(),
        ),
        joined,
        "repair must retain the closed pair across rig's carrier boundary"
    );
}

#[test]
fn tool_argument_repair_follows_the_lean_repair_model() {
    use gents_lean_vocab::families::prompt_assembly_repair_args::{ArgsInput, ArgsOutput};
    gents_lean_vocab::families::prompt_assembly_repair_args::replay(|case: &ArgsInput| {
        let input = repair_vector(&case.input);
        if case.payload_only {
            assert!(input.is_object(), "{case:?}");
        }
        let once = repaired_arguments(input);
        let twice = repaired_arguments(once.clone());
        assert_eq!(twice, once, "{case:?}");
        ArgsOutput {
            expected: repair_shape(&once),
            expected_twice: repair_shape(&twice),
        }
    });
}

fn repair_vector(name: &str) -> serde_json::Value {
    match name {
        "object:raw" => serde_json::json!({"k": "line\nbreak"}),
        "object:empty" => serde_json::json!({}),
        "object:sanitized" => serde_json::json!({"k": "no break"}),
        "str:object:raw" => serde_json::Value::String("{\"k\": \"line\\nbreak\"}".to_string()),
        "str:unparsed" => serde_json::Value::String("not json at all".to_string()),
        "array" => serde_json::json!([1, 2]),
        "scalar" => serde_json::json!(123),
        "null" => serde_json::Value::Null,
        other => panic!("generated repair case names an unmodeled shape: {other}"),
    }
}

fn repair_shape(value: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = value else {
        panic!("repair must always yield an object, got {value:?}");
    };
    if map.is_empty() {
        return "object:empty".to_string();
    }
    if has_raw_leaf(value) {
        "object:raw".to_string()
    } else {
        "object:sanitized".to_string()
    }
}

fn has_raw_leaf(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains('\n'),
        serde_json::Value::Array(values) => values.iter().any(has_raw_leaf),
        serde_json::Value::Object(map) => map.values().any(has_raw_leaf),
        _ => false,
    }
}

fn repaired_arguments(arguments: serde_json::Value) -> serde_json::Value {
    let history = vec![
        Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    raw_arguments: None,
                    id: "call-1".to_string(),
                    call_id: Some("call-1".to_string()),
                    function: gents_protocol::message::ToolFunction {
                        name: "echo".to_string(),
                        arguments,
                    },
                    signature: None,
                    additional_params: None,
                },
            )],
        },
        Message::User {
            content: vec![UserContent::ToolResult(
                gents_protocol::message::ToolResult {
                    id: "call-1".to_string(),
                    call_id: Some("call-1".to_string()),
                    content: vec![ToolResultContent::Text(gents_protocol::message::Text {
                        text: "call-1-result".to_string(),
                    })],
                },
            )],
        },
    ];
    let mut history = history
        .into_iter()
        .map(TaggedMessage::unassociated)
        .collect();
    let mut new_messages = Vec::new();
    super::repair::repair_provider_input(&mut history, &mut new_messages).unwrap();
    let repaired = history
        .iter()
        .chain(new_messages.iter())
        .map(|row| &row.message)
        .collect::<Vec<_>>();
    let [Message::Assistant { content, .. }, Message::User { .. }] = repaired.as_slice() else {
        panic!("repair must rewrite payloads only, never rows: {repaired:?}");
    };
    let [AssistantContent::ToolCall(tool_call)] = content.as_slice() else {
        panic!("repair dropped the tool call: {content:?}");
    };
    tool_call.function.arguments.clone()
}
