use super::*;

/// Stands in for an image part on a provider whose wire cannot carry one
/// inside a tool result.
pub const TOOL_RESULT_IMAGE_OMITTED: &str =
    "[image omitted: this model's provider does not accept images in tool results]";

/// A dispatched tool's output as the provider receives it. Output with no
/// image part is bounded whole and then split, as it always was. Output that
/// splits into image parts (`{"response", "parts"}`, the plugin ABI in
/// `gents::plugin`) is split first and each text part is bounded. Every image
/// passes whole where `profile` carries tool-result images, because a
/// byte-bounded image is not an image; the tool's own output limit bounds it.
/// Elsewhere each image becomes [`TOOL_RESULT_IMAGE_OMITTED`]: the OpenAI-family
/// converters refuse a whole request over one tool-result image.
pub(super) fn bounded_tool_result(
    profile: crate::provider_input::ProviderInputProfile,
    tool_name: &str,
    output: &str,
) -> Vec<ToolResultContent> {
    let mode = tool_result_truncation_mode(tool_name);
    let limits = TruncationLimits::default();
    let content = ToolResultContent::from_tool_output(output);
    if !content
        .iter()
        .any(|part| matches!(part, ToolResultContent::Image(_)))
    {
        let (bounded, _, truncated) = truncate_text(output, mode, &limits);
        return if truncated {
            ToolResultContent::from_tool_output(bounded)
        } else {
            content
        };
    }
    content
        .into_iter()
        .map(|part| match part {
            ToolResultContent::Text(text) => {
                ToolResultContent::text(truncate_text(&text.text, mode, &limits).0)
            }
            image if profile.carries_tool_result_images() => image,
            ToolResultContent::Image(_) => ToolResultContent::text(TOOL_RESULT_IMAGE_OMITTED),
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
    // Thread the assistant turn (text + reasoning + tool calls) ahead of its
    // tool results, matching rig's history ordering. Carry the provider
    // message id (captured into `stream.message_id` from the stream's
    // `MessageId` event) onto the threaded message — rig threads this same id,
    // and OpenAI Responses / ChatGPT Codex follow-up requests reference prior
    // `msg_` ids, so dropping it breaks them.
    if let Some(mut assistant_message) = accumulator.take_message() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_input::ProviderInputProfile;

    fn image_output(data_len: usize, response_len: usize) -> String {
        serde_json::json!({
            "response": "r".repeat(response_len),
            "parts": [{"type": "image", "data": "a".repeat(data_len), "mimeType": "image/png"}],
        })
        .to_string()
    }

    #[test]
    fn output_without_images_is_bounded_whole_then_split() {
        let small = r#"{"response":"ok"}"#;
        assert_eq!(
            bounded_tool_result(ProviderInputProfile::ClaudeMessages, "plugin", small),
            ToolResultContent::from_tool_output(small)
        );
        let big = "x".repeat(200_000);
        let (bounded, _, truncated) = truncate_text(
            &big,
            crate::truncation::TruncationMode::Head,
            &TruncationLimits::default(),
        );
        assert!(truncated);
        assert_eq!(
            bounded_tool_result(ProviderInputProfile::ClaudeMessages, "plugin", &big),
            vec![ToolResultContent::text(bounded)]
        );
    }

    #[test]
    fn images_pass_whole_and_only_text_parts_are_bounded() {
        let limits = TruncationLimits::default();
        let content = bounded_tool_result(
            ProviderInputProfile::ClaudeMessages,
            "plugin",
            &image_output(400_000, 200_000),
        );
        let [ToolResultContent::Text(text), ToolResultContent::Image(image)] = content.as_slice()
        else {
            panic!("one text part and one image part: {content:?}");
        };
        assert!(text.text.len() < 200_000 && text.text.len() <= limits.max_bytes + 200);
        let gents_protocol::message::DocumentSourceKind::Base64(data) = &image.data else {
            panic!("base64 image data");
        };
        assert_eq!(data.len(), 400_000);
    }

    #[test]
    fn a_provider_without_tool_result_images_gets_a_note_instead() {
        let content = bounded_tool_result(
            ProviderInputProfile::OpenAiChatCompletions,
            "plugin",
            &image_output(400_000, 10),
        );
        assert_eq!(
            content,
            vec![
                ToolResultContent::text(format!("\"{}\"", "r".repeat(10))),
                ToolResultContent::text(TOOL_RESULT_IMAGE_OMITTED),
            ]
        );
    }

    /// `carries_tool_result_images` matches what each wire's converter
    /// accepts: every profile projects a bounded image result.
    #[cfg(feature = "native")]
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
                .unwrap_or_else(|error| panic!("{:?}: {error:#}", counter.profile()));
        }
    }
}
