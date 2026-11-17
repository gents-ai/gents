use gents_protocol::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
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

#[test]
fn stubbing_receipts_count_only_replacements_and_preserve_projection_bytes() {
    let mut large = scoped_result(1);
    if let Message::User { content } = &mut large {
        let UserContent::ToolResult(result) = &mut content[0] else {
            unreachable!()
        };
        result.content = vec![ToolResultContent::text("é".repeat(2_000))];
    }
    let input = vec![
        Message::Assistant {
            request_id: None,
            id: None,
            content: vec![scoped_call(1), scoped_call(2)],
        },
        large,
        scoped_result(2),
    ];
    let legacy = super::provider_view(ProviderInputProfile::OpenAiChatCompletions, input.clone()).0;
    let (projected, _, receipts) =
        super::provider_view_accounted(ProviderInputProfile::OpenAiChatCompletions, input);
    assert_eq!(
        projected, legacy,
        "accounting cannot change stubbing policy or provider bytes"
    );
    let figures = super::history::replacement_figures(&receipts);
    assert_eq!(figures.stubbed_results, 1);
    assert_eq!(figures.original_bytes, 4_000, "UTF-8 bytes, not characters");
    assert!(figures.bytes_removed() > 0);
    let (_, _, reapplied) = super::provider_view_accounted(
        ProviderInputProfile::OpenAiChatCompletions,
        projected.clone(),
    );
    assert!(
        reapplied.is_empty(),
        "pre-existing or spoofed literal stubs have no replacement receipt"
    );
    let tagged = projected
        .into_iter()
        .map(crate::loop_stream::TaggedMessage::unassociated)
        .collect();
    let spoofed = crate::loop_stream::provider_view_tagged(tagged).unwrap();
    assert!(spoofed.iter().all(|row| row.history_stubs.is_empty()));
}

#[test]
fn stubbing_receipts_drop_orphans_and_survive_reprojection_without_accumulating() {
    let mut result = scoped_result(1);
    if let Message::User { content } = &mut result {
        let UserContent::ToolResult(result) = &mut content[0] else {
            unreachable!()
        };
        result.content = vec![ToolResultContent::text("x".repeat(2_000))];
    }
    let (_, _, orphan_receipts) = super::provider_view_accounted(
        ProviderInputProfile::OpenAiChatCompletions,
        vec![result.clone()],
    );
    assert!(orphan_receipts.is_empty());
    let input = vec![
        Message::Assistant {
            request_id: None,
            id: None,
            content: vec![scoped_call(1)],
        },
        result,
    ];
    let once = crate::loop_stream::provider_view_tagged(
        input
            .into_iter()
            .map(crate::loop_stream::TaggedMessage::unassociated)
            .collect(),
    )
    .unwrap();
    let twice = crate::loop_stream::provider_view_tagged(once.clone()).unwrap();
    assert_eq!(once, twice);
    assert_eq!(
        twice
            .iter()
            .map(|row| row.history_stubs.len())
            .sum::<usize>(),
        1
    );
    let sanitized = crate::loop_stream::sanitize_tagged_history(twice).unwrap();
    assert_eq!(
        sanitized
            .iter()
            .map(|row| row.history_stubs.len())
            .sum::<usize>(),
        1
    );
}
