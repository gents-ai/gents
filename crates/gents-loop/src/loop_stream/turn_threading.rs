use super::*;

pub const TOOL_RESULT_IMAGE_OMITTED: &str =
    "[image omitted: this model's provider does not accept images in tool results]";

pub const TOOL_RESULT_IMAGE_NOT_REPLAYED: &str =
    "[image omitted: images from earlier tool calls are not replayed; call the tool again to see it]";

pub(super) fn bounded_tool_result(
    profile: crate::provider_input::ProviderInputProfile,
    tool_name: &str,
    output: &str,
) -> Vec<ToolResultContent> {
    let content = ToolResultContent::from_tool_output(output);
    if !content
        .iter()
        .any(|part| matches!(part, ToolResultContent::Image(_)))
    {
        let (bounded, _, truncated) = truncate_text(
            output,
            tool_result_truncation_mode(tool_name),
            &TruncationLimits::default(),
        );
        return if truncated {
            ToolResultContent::from_tool_output(bounded)
        } else {
            content
        };
    }
    let note = (!profile.carries_tool_result_images()).then_some(TOOL_RESULT_IMAGE_OMITTED);
    bound_split_parts(tool_name, content, note, &TruncationLimits::default())
}

pub fn replayed_tool_result_text(
    tool_name: &str,
    output: &str,
    limits: &TruncationLimits,
) -> Option<String> {
    let content = ToolResultContent::from_tool_output(output);
    if !content
        .iter()
        .any(|part| matches!(part, ToolResultContent::Image(_)))
    {
        return None;
    }
    let lines: Vec<String> = bound_split_parts(
        tool_name,
        content,
        Some(TOOL_RESULT_IMAGE_NOT_REPLAYED),
        limits,
    )
    .into_iter()
    .filter_map(|part| match part {
        ToolResultContent::Text(text) => Some(text.text),
        ToolResultContent::Image(_) => None,
    })
    .collect();
    Some(lines.join("\n"))
}

fn bound_split_parts(
    tool_name: &str,
    content: Vec<ToolResultContent>,
    image_note: Option<&str>,
    limits: &TruncationLimits,
) -> Vec<ToolResultContent> {
    let mode = tool_result_truncation_mode(tool_name);
    content
        .into_iter()
        .map(|part| match (part, image_note) {
            (ToolResultContent::Text(text), _) => {
                ToolResultContent::text(truncate_text(&text.text, mode, limits).0)
            }
            (image, None) => image,
            (ToolResultContent::Image(_), Some(note)) => ToolResultContent::text(note),
        })
        .collect()
}

pub(super) fn close_streaming_turn(
    new_messages: &mut Vec<TaggedMessage>,
    accumulator: &mut AssistantTurnAccumulator,
    message_id: Option<String>,
    assistant_source: Option<crate::claude_messages_body::ReplayTag>,
    pending_results: Vec<(ToolCall, String, Vec<ToolResultContent>)>,
) -> Vec<LoopStreamItem> {
    if let Some(mut assistant_message) = accumulator.take_message() {
        // Follow-up Responses requests reference the provider message id.
        if let Message::Assistant { id, .. } = &mut assistant_message {
            *id = message_id;
        }
        new_messages.push(TaggedMessage {
            message: assistant_message,
            source: assistant_source,
            physical_header: None,
            block_indices: Vec::new(),
        });
    }

    pending_results
        .into_iter()
        .map(|(tool_call, internal_call_id, content)| {
            let user_content = match tool_call.call_id.clone() {
                Some(call_id) => UserContent::tool_result_with_call_id(
                    tool_call.id.clone(),
                    call_id,
                    content.clone(),
                ),
                None => UserContent::tool_result(tool_call.id.clone(), content.clone()),
            };
            new_messages.push(TaggedMessage::unassociated(Message::User {
                content: vec![user_content],
            }));

            let tool_result = ToolResult {
                id: tool_call.id.clone(),
                call_id: tool_call.call_id.clone(),
                content,
            };
            LoopStreamItem::ToolResult {
                tool_result,
                internal_call_id,
            }
        })
        .collect()
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use gents_protocol::error_chain::ErrorChain;

    fn image_output(data_len: usize, response_len: usize) -> String {
        serde_json::json!({
            "response": "r".repeat(response_len),
            "parts": [{"type": "image", "data": "a".repeat(data_len), "mimeType": "image/png"}],
        })
        .to_string()
    }
    #[test]
    fn every_profile_projects_a_bounded_image_result() {
        use crate::backend_provider::BackendProviderKind as Provider;
        use crate::openai_wire::OpenAiWireApi as Wire;
        use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
        for (provider, wire) in [
            (Provider::OpenAiCompatible, Wire::ChatCompletions),
            (Provider::OpenAiCompatible, Wire::Responses),
            (Provider::OpenRouter, Wire::ChatCompletions),
            (Provider::ChatGptCodex, Wire::Responses),
            (Provider::XaiGrokOAuth, Wire::Responses),
            (Provider::AnthropicApiKey, Wire::ChatCompletions),
        ] {
            let counter = crate::provider_input::ProviderInputCounter::new(provider, wire, "m");
            let mut call = ToolCall::new(
                "c1".into(),
                ToolFunction {
                    name: "plugin".into(),
                    arguments: serde_json::json!({}),
                },
            );
            call.call_id = Some("call_1".into());
            let content = bounded_tool_result(counter.profile(), "plugin", &image_output(64, 4));
            let messages = vec![
                Message::user("draw it"),
                Message::Assistant {
                    id: None,
                    content: vec![AssistantContent::ToolCall(call)],
                },
                Message::User {
                    content: vec![UserContent::tool_result_with_call_id(
                        "c1".to_string(),
                        "call_1".to_string(),
                        content,
                    )],
                },
            ];
            counter
                .estimate_message_request(&messages)
                .unwrap_or_else(|error| panic!("{:?}: {}", counter.profile(), ErrorChain(&error)));
        }
    }
}
