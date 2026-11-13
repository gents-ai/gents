use gents_protocol::message::{
    AssistantContent, Message, ToolCall, ToolFunction, ToolResult, ToolResultContent, Text,
    UserContent,
};

use super::sanitize_history_for_provider;
use crate::provider_input::ProviderInputProfile;

fn scoped_call(id: u64) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        raw_arguments: None,
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

fn scoped_result(id: u64) -> Message {
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

fn scenario_messages(
    steps: &[gents_lean_vocab::families::provider_view_call_keys_sanitize::StepWire],
) -> Vec<Message> {
    steps
        .iter()
        .map(|step| match step.kind.as_str() {
            "calls" => Message::Assistant {
                request_id: None,
                id: None,
                content: step.call_ids.iter().map(|id| scoped_call(*id)).collect(),
            },
            "result" => scoped_result(step.call_ids.first().copied().unwrap_or(0)),
            other => panic!("unknown scenario step kind {other:?}"),
        })
        .collect()
}

fn calls_and_results(messages: &[Message]) -> (usize, usize) {
    let mut calls = 0;
    let mut results = 0;
    for message in messages {
        match message {
            Message::Assistant { content, .. } => {
                calls += content
                    .iter()
                    .filter(|item| matches!(item, AssistantContent::ToolCall(_)))
                    .count();
            }
            Message::User { content } => {
                results += content
                    .iter()
                    .filter(|item| matches!(item, UserContent::ToolResult(_)))
                    .count();
            }
            Message::System { .. } => {}
        }
    }
    (calls, results)
}

#[test]
fn provider_view_call_keys_match_the_lean_turn_model() {
    gents_lean_vocab::families::provider_view_call_keys_sanitize::replay(
        |input: &gents_lean_vocab::families::provider_view_call_keys_sanitize::Scenario| {
            let messages = scenario_messages(&input.steps);
            let out = sanitize_history_for_provider(
                ProviderInputProfile::OpenAiChatCompletions,
                messages.clone(),
            );
            let (call_count, result_count) = calls_and_results(&out);
            gents_lean_vocab::families::provider_view_call_keys_sanitize::ViewOutput {
                identity: out == messages,
                call_count: call_count as u64,
                result_count: result_count as u64,
            }
        },
    );
}
