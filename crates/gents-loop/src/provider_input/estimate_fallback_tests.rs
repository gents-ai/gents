use gents_protocol::message::{
    AssistantContent, Message, Reasoning, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use serde_json::json;

use super::*;

fn xai_responses() -> ProviderInputCounter {
    ProviderInputCounter::new(
        BackendProviderKind::XaiGrokOAuth,
        OpenAiWireApi::Responses,
        "grok-4.7",
    )
}

fn grok_tool_round_trip(reasoning: Option<Reasoning>) -> Vec<Message> {
    let mut assistant = reasoning
        .map(AssistantContent::Reasoning)
        .into_iter()
        .collect::<Vec<_>>();
    // xAI items carry only `id`; the stored call has no `call_id`.
    assistant.push(AssistantContent::ToolCall(ToolCall {
        id: "fc_1".into(),
        call_id: None,
        function: ToolFunction {
            name: "inspect".into(),
            arguments: json!({"path": "src/main.rs"}),
        },
        signature: None,
        additional_params: None,
    }));
    vec![
        Message::user("look at main"),
        Message::Assistant {
            id: None,
            content: assistant,
        },
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "fc_1".into(),
                call_id: None,
                content: vec![ToolResultContent::text("fn main() {}".repeat(50))],
            })],
        },
        Message::user("continue"),
    ]
}

#[test]
fn xai_tool_round_trip_without_call_id_is_estimated_on_the_wire() {
    let counter = xai_responses();
    let messages = grok_tool_round_trip(None);
    let tokens = counter
        .project_message_request(&messages)
        .expect("normalization fills call_id from id, so the wire projection succeeds");
    assert!(tokens > 0);
    assert_eq!(counter.estimate_message_request(&messages).unwrap(), tokens);
}

#[test]
fn unprojectable_history_falls_back_to_a_byte_estimate_instead_of_failing() {
    let counter = xai_responses();
    // A reasoning item without an OpenAI-generated id: tool-call
    // normalization does not touch it, and the Responses conversion rejects
    // it, as the live xAI compaction failure did.
    let messages = grok_tool_round_trip(Some(Reasoning::new("thinking about main")));
    let projection_error = counter
        .project_message_request(&messages)
        .expect_err("the Responses projection rejects a reasoning item without an id");
    let chain = gents_protocol::error_chain::ErrorChain(&projection_error).to_string();
    assert!(chain.contains("reasoning"), "{chain}");

    let tokens = counter
        .estimate_message_request(&messages)
        .expect("an estimate never fails the request");
    assert_eq!(tokens, message_byte_estimate(&messages));
    assert!(tokens > 0);
}
