use rig::completion::message::{
    AssistantContent, Message, Reasoning, Text, ToolResult, ToolResultContent, UserContent,
};
use rig::one_or_many::OneOrMany;

use super::{
    decode_message, extract_message_reasoning, render_transcript, MessageRow, ResponseRow,
};

#[test]
fn transcript_skips_tool_result_only_user_messages() {
    let tool_result_message = Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: "tool-1".to_string(),
            call_id: None,
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: "{\"path\":\"src\"}".to_string(),
            })),
        })),
    };
    let assistant_message = Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text {
            text: "Final answer".to_string(),
        })),
    };

    let messages = vec![
        MessageRow {
            sequence: 1,
            role: "user".to_string(),
            content: serde_json::to_string(&tool_result_message).unwrap(),
        },
        MessageRow {
            sequence: 2,
            role: "assistant".to_string(),
            content: serde_json::to_string(&assistant_message).unwrap(),
        },
    ];

    let transcript = render_transcript(&messages, None);
    assert!(
        !transcript.contains("You\n{\"path\":\"src\"}"),
        "{transcript}"
    );
    assert!(
        transcript.contains("Assistant\nFinal answer"),
        "{transcript}"
    );
}

#[test]
fn reasoning_extraction_reads_persisted_assistant_reasoning() {
    let message = Message::Assistant {
        id: None,
        content: OneOrMany::many(vec![
            AssistantContent::Reasoning(Reasoning::new("Need to inspect the CLI flow first")),
            AssistantContent::Text(Text {
                text: "I checked the CLI flow.".to_string(),
            }),
        ])
        .unwrap(),
    };

    let decoded = decode_message("assistant", &serde_json::to_string(&message).unwrap());
    let reasoning = extract_message_reasoning(&decoded).expect("reasoning");
    assert!(reasoning.contains("Need to inspect the CLI flow first"));
}

#[test]
fn transcript_includes_streaming_draft_and_error_response() {
    let draft = render_transcript(
        &[],
        Some(&ResponseRow {
            request_id: "req-1".to_string(),
            status: "streaming".to_string(),
            content: "draft answer".to_string(),
            reasoning: String::new(),
            error_message: String::new(),
        }),
    );
    assert!(draft.contains("Assistant (draft)\ndraft answer"), "{draft}");

    let error = render_transcript(
        &[],
        Some(&ResponseRow {
            request_id: "req-2".to_string(),
            status: "error".to_string(),
            content: String::new(),
            reasoning: String::new(),
            error_message: "provider timeout".to_string(),
        }),
    );
    assert!(
        error.contains("Assistant (error)\nprovider timeout"),
        "{error}"
    );
}
