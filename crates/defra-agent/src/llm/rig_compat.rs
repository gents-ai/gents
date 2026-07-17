//! Converters between Defra-native [`crate::llm`] types and rig's, used only at
//! the provider/parsing boundary (Layer A). Deleted once Layer A is owned.
//!
//! These are free functions rather than `From` impls: rig's types are foreign,
//! so `impl From<Native> for RigType` would violate the orphan rule.

use anyhow::{Context, Result};
use serde_json::Value;

use super::tool::ToolDefinition;
use super::ToolChoice;

/// Convert a native [`ToolDefinition`] into rig's, for the outgoing completion
/// request's tool list.
pub(crate) fn to_rig_tool_definition(def: &ToolDefinition) -> rig::completion::ToolDefinition {
    rig::completion::ToolDefinition {
        name: def.name.clone(),
        description: def.description.clone(),
        parameters: def.parameters.clone(),
    }
}

/// Convert a native [`ToolChoice`] into rig's, for the outgoing completion request.
pub(crate) fn to_rig_tool_choice(choice: &ToolChoice) -> rig::message::ToolChoice {
    match choice {
        ToolChoice::Auto => rig::message::ToolChoice::Auto,
        ToolChoice::None => rig::message::ToolChoice::None,
        ToolChoice::Required => rig::message::ToolChoice::Required,
        ToolChoice::Specific { function_names } => rig::message::ToolChoice::Specific {
            function_names: function_names.clone(),
        },
    }
}

pub(crate) fn rendered_completion_request(
    context: &crate::rendered_request::RenderedRequestContext,
    turn_index: usize,
    attempt: u32,
    request: &rig::completion::CompletionRequest,
) -> Result<crate::rendered_request::RenderedCompletionRequest> {
    let request_json = provider_request_json(
        &context.model_name,
        context.source,
        context.normalize_responses_wire,
        request,
    )?;
    let messages_json = provider_messages(&request_json, context.source);
    let tools_json = request_json
        .get("tools")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let tool_choice_json = request_json
        .get("tool_choice")
        .cloned()
        .unwrap_or(Value::Null);
    let sampling_json = crate::rendered_request::sampling_json(
        request.temperature,
        request.max_tokens,
        request.additional_params.clone(),
    );

    crate::rendered_request::build_rendered_completion_request(
        context,
        turn_index,
        attempt,
        request_json,
        messages_json,
        tools_json,
        tool_choice_json,
        sampling_json,
    )
}

fn provider_request_json(
    model_name: &str,
    source: crate::rendered_request::RenderedRequestSource,
    normalize_responses_wire: bool,
    request: &rig::completion::CompletionRequest,
) -> Result<Value> {
    match source {
        crate::rendered_request::RenderedRequestSource::OpenAiResponses => {
            let provider_request =
                rig::providers::openai::responses_api::CompletionRequest::try_from((
                    model_name.to_string(),
                    request.clone(),
                ))
                .context("rendering OpenAI Responses request")?;
            let mut value = serde_json::to_value(provider_request)
                .context("encoding OpenAI Responses request")?;
            if normalize_responses_wire {
                crate::llm::responses_normalize::normalize_responses_assistant_items(&mut value);
            }
            Ok(value)
        }
        crate::rendered_request::RenderedRequestSource::OpenAiChatCompletions => {
            let provider_request = rig::providers::openai::CompletionRequest::try_from((
                model_name.to_string(),
                request.clone(),
            ))
            .context("rendering OpenAI Chat Completions request")?;
            serde_json::to_value(provider_request)
                .context("encoding OpenAI Chat Completions request")
        }
    }
}

fn provider_messages(
    request_json: &Value,
    source: crate::rendered_request::RenderedRequestSource,
) -> Value {
    match source {
        crate::rendered_request::RenderedRequestSource::OpenAiResponses => request_json
            .get("input")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        crate::rendered_request::RenderedRequestSource::OpenAiChatCompletions => request_json
            .get("messages")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    }
}

// ===== Message family (Layer B seam) =====
//
// Outbound (`to_rig_*`): native history → rig `CompletionRequest` messages at
// `loop_stream::build_request`, plus rig `PromptError` payloads and the rig
// stream items the loop yields (decision D3 keeps the consumer contract).
// Inbound (`from_rig_*`): rig streamed content → native at the accumulate/
// persist seam (`AssistantTurnAccumulator`, `StreamProcessor`).
//
// Content lists are non-empty by convention (the sanitizer drops empty
// messages; the accumulator returns `None` instead of empty turns). The
// `OneOrMany` constructions fall back to a single empty text block rather
// than panic so a convention violation degrades to a harmless empty message.

use super::message;

pub(crate) fn to_rig_messages(messages: &[message::Message]) -> Vec<rig::completion::Message> {
    messages.iter().map(to_rig_message).collect()
}

pub(crate) fn to_rig_message(msg: &message::Message) -> rig::completion::Message {
    match msg {
        message::Message::System { content } => rig::completion::Message::System {
            content: content.clone(),
        },
        message::Message::User { content } => rig::completion::Message::User {
            content: rig::one_or_many::OneOrMany::many(
                content.iter().map(to_rig_user_content).collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| {
                rig::one_or_many::OneOrMany::one(rig::completion::message::UserContent::text(""))
            }),
        },
        message::Message::Assistant { id, content } => rig::completion::Message::Assistant {
            id: id.clone(),
            content: rig::one_or_many::OneOrMany::many(
                content
                    .iter()
                    .map(to_rig_assistant_content)
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| {
                rig::one_or_many::OneOrMany::one(rig::completion::message::AssistantContent::text(
                    "",
                ))
            }),
        },
    }
}

pub(crate) fn to_rig_user_content(
    content: &message::UserContent,
) -> rig::completion::message::UserContent {
    use rig::completion::message::UserContent as R;
    match content {
        message::UserContent::Text(text) => R::Text(to_rig_text(text)),
        message::UserContent::ToolResult(result) => R::ToolResult(to_rig_tool_result(result)),
        message::UserContent::Image(image) => R::Image(to_rig_image(image)),
        message::UserContent::Audio(audio) => R::Audio(to_rig_audio(audio)),
        message::UserContent::Video(video) => R::Video(to_rig_video(video)),
        message::UserContent::Document(document) => R::Document(to_rig_document(document)),
    }
}

pub(crate) fn to_rig_assistant_content(
    content: &message::AssistantContent,
) -> rig::completion::message::AssistantContent {
    use rig::completion::message::AssistantContent as R;
    match content {
        message::AssistantContent::Text(text) => R::Text(to_rig_text(text)),
        message::AssistantContent::ToolCall(call) => R::ToolCall(to_rig_tool_call(call)),
        message::AssistantContent::Reasoning(reasoning) => {
            R::Reasoning(to_rig_reasoning(reasoning))
        }
        message::AssistantContent::Image(image) => R::Image(to_rig_image(image)),
    }
}

pub(crate) fn to_rig_text(text: &message::Text) -> rig::completion::message::Text {
    rig::completion::message::Text {
        text: text.text.clone(),
    }
}

pub(crate) fn to_rig_tool_call(call: &message::ToolCall) -> rig::completion::message::ToolCall {
    rig::completion::message::ToolCall {
        id: call.id.clone(),
        call_id: call.call_id.clone(),
        function: rig::completion::message::ToolFunction {
            name: call.function.name.clone(),
            // Egress half of the #589/#590 argument-shape boundary: the
            // durable transcript is permissive, so already-persisted poison
            // (a non-object `arguments` Value) is normalized here at request
            // build — a jammed session self-heals on its next turn.
            arguments: super::tool::normalize_tool_call_arguments(
                "egress",
                &call.function.name,
                &call.function.arguments,
            ),
        },
        signature: call.signature.clone(),
        additional_params: call.additional_params.clone(),
    }
}

pub(crate) fn to_rig_tool_result(
    result: &message::ToolResult,
) -> rig::completion::message::ToolResult {
    rig::completion::message::ToolResult {
        id: result.id.clone(),
        call_id: result.call_id.clone(),
        content: rig::one_or_many::OneOrMany::many(
            result
                .content
                .iter()
                .map(to_rig_tool_result_content)
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| {
            rig::one_or_many::OneOrMany::one(rig::completion::message::ToolResultContent::text(""))
        }),
    }
}

pub(crate) fn to_rig_tool_result_content(
    content: &message::ToolResultContent,
) -> rig::completion::message::ToolResultContent {
    match content {
        message::ToolResultContent::Text(text) => {
            rig::completion::message::ToolResultContent::Text(to_rig_text(text))
        }
        message::ToolResultContent::Image(image) => {
            rig::completion::message::ToolResultContent::Image(to_rig_image(image))
        }
    }
}

pub(crate) fn to_rig_reasoning(
    reasoning: &message::Reasoning,
) -> rig::completion::message::Reasoning {
    // rig's `Reasoning` is #[non_exhaustive]; construct via `new` and assign
    // the public fields.
    let mut rig_reasoning = rig::completion::message::Reasoning::new("");
    rig_reasoning.id = reasoning.id.clone();
    rig_reasoning.content = reasoning
        .content
        .iter()
        .map(|item| match item {
            message::ReasoningContent::Text { text, signature } => {
                rig::completion::message::ReasoningContent::Text {
                    text: text.clone(),
                    signature: signature.clone(),
                }
            }
            message::ReasoningContent::Encrypted(data) => {
                rig::completion::message::ReasoningContent::Encrypted(data.clone())
            }
            message::ReasoningContent::Redacted { data } => {
                rig::completion::message::ReasoningContent::Redacted { data: data.clone() }
            }
            message::ReasoningContent::Summary(text) => {
                rig::completion::message::ReasoningContent::Summary(text.clone())
            }
        })
        .collect();
    rig_reasoning
}

fn to_rig_source_kind(
    kind: &message::DocumentSourceKind,
) -> rig::completion::message::DocumentSourceKind {
    use rig::completion::message::DocumentSourceKind as R;
    match kind {
        message::DocumentSourceKind::Url(url) => R::Url(url.clone()),
        message::DocumentSourceKind::Base64(data) => R::Base64(data.clone()),
        message::DocumentSourceKind::Raw(bytes) => R::Raw(bytes.clone()),
        message::DocumentSourceKind::String(text) => R::String(text.clone()),
        message::DocumentSourceKind::Unknown => R::Unknown,
    }
}

fn to_rig_image(image: &message::Image) -> rig::completion::message::Image {
    rig::completion::message::Image {
        data: to_rig_source_kind(&image.data),
        media_type: image.media_type.as_ref().map(|m| {
            use rig::completion::message::ImageMediaType as R;
            match m {
                message::ImageMediaType::JPEG => R::JPEG,
                message::ImageMediaType::PNG => R::PNG,
                message::ImageMediaType::GIF => R::GIF,
                message::ImageMediaType::WEBP => R::WEBP,
                message::ImageMediaType::HEIC => R::HEIC,
                message::ImageMediaType::HEIF => R::HEIF,
                message::ImageMediaType::SVG => R::SVG,
            }
        }),
        detail: image.detail.as_ref().map(|d| {
            use rig::completion::message::ImageDetail as R;
            match d {
                message::ImageDetail::Low => R::Low,
                message::ImageDetail::High => R::High,
                message::ImageDetail::Auto => R::Auto,
            }
        }),
        additional_params: image.additional_params.clone(),
    }
}

fn to_rig_audio(audio: &message::Audio) -> rig::completion::message::Audio {
    rig::completion::message::Audio {
        data: to_rig_source_kind(&audio.data),
        media_type: audio.media_type.as_ref().map(|m| {
            use rig::completion::message::AudioMediaType as R;
            match m {
                message::AudioMediaType::WAV => R::WAV,
                message::AudioMediaType::MP3 => R::MP3,
                message::AudioMediaType::AIFF => R::AIFF,
                message::AudioMediaType::AAC => R::AAC,
                message::AudioMediaType::OGG => R::OGG,
                message::AudioMediaType::FLAC => R::FLAC,
                message::AudioMediaType::M4A => R::M4A,
                message::AudioMediaType::PCM16 => R::PCM16,
                message::AudioMediaType::PCM24 => R::PCM24,
            }
        }),
        additional_params: audio.additional_params.clone(),
    }
}

fn to_rig_video(video: &message::Video) -> rig::completion::message::Video {
    rig::completion::message::Video {
        data: to_rig_source_kind(&video.data),
        media_type: video.media_type.as_ref().map(|m| {
            use rig::completion::message::VideoMediaType as R;
            match m {
                message::VideoMediaType::AVI => R::AVI,
                message::VideoMediaType::MP4 => R::MP4,
                message::VideoMediaType::MPEG => R::MPEG,
                message::VideoMediaType::MOV => R::MOV,
                message::VideoMediaType::WEBM => R::WEBM,
            }
        }),
        additional_params: video.additional_params.clone(),
    }
}

fn to_rig_document(document: &message::Document) -> rig::completion::message::Document {
    rig::completion::message::Document {
        data: to_rig_source_kind(&document.data),
        media_type: document.media_type.as_ref().map(|m| {
            use rig::completion::message::DocumentMediaType as R;
            match m {
                message::DocumentMediaType::PDF => R::PDF,
                message::DocumentMediaType::TXT => R::TXT,
                message::DocumentMediaType::RTF => R::RTF,
                message::DocumentMediaType::HTML => R::HTML,
                message::DocumentMediaType::CSS => R::CSS,
                message::DocumentMediaType::MARKDOWN => R::MARKDOWN,
                message::DocumentMediaType::CSV => R::CSV,
                message::DocumentMediaType::XML => R::XML,
                message::DocumentMediaType::Javascript => R::Javascript,
                message::DocumentMediaType::Python => R::Python,
            }
        }),
        additional_params: document.additional_params.clone(),
    }
}

pub(crate) fn from_rig_tool_call(call: &rig::completion::message::ToolCall) -> message::ToolCall {
    message::ToolCall {
        id: call.id.clone(),
        call_id: call.call_id.clone(),
        function: message::ToolFunction {
            name: call.function.name.clone(),
            // Ingest half of the #589/#590 argument-shape boundary: a wire
            // payload the provider parser could not shape into an object (a
            // raw corrupt string, `[]`, a scalar) is normalized before it can
            // be accumulated into durable history. Dispatch reads the RAW rig
            // value separately (`loop_stream`), so an unsalvageable payload
            // still fails `parse_tool_args` and terminalizes
            // `failed(ArgumentInvalid)` with the model notified.
            arguments: super::tool::normalize_tool_call_arguments(
                "ingest",
                &call.function.name,
                &call.function.arguments,
            ),
        },
        signature: call.signature.clone(),
        additional_params: call.additional_params.clone(),
    }
}

pub(crate) fn from_rig_reasoning(
    reasoning: &rig::completion::message::Reasoning,
) -> message::Reasoning {
    message::Reasoning {
        id: reasoning.id.clone(),
        content: reasoning
            .content
            .iter()
            .map(|item| match item {
                rig::completion::message::ReasoningContent::Text { text, signature } => {
                    message::ReasoningContent::Text {
                        text: text.clone(),
                        signature: signature.clone(),
                    }
                }
                rig::completion::message::ReasoningContent::Encrypted(data) => {
                    message::ReasoningContent::Encrypted(data.clone())
                }
                rig::completion::message::ReasoningContent::Redacted { data } => {
                    message::ReasoningContent::Redacted { data: data.clone() }
                }
                rig::completion::message::ReasoningContent::Summary(text) => {
                    message::ReasoningContent::Summary(text.clone())
                }
                other => {
                    tracing::warn!(
                        ?other,
                        "unsupported rig reasoning content stubbed at the inbound seam"
                    );
                    message::ReasoningContent::Summary(format!(
                        "[unsupported reasoning content: {other:?}]"
                    ))
                }
            })
            .collect(),
    }
}

pub(crate) fn from_rig_tool_result(
    result: &rig::completion::message::ToolResult,
) -> message::ToolResult {
    message::ToolResult {
        id: result.id.clone(),
        call_id: result.call_id.clone(),
        content: result
            .content
            .iter()
            .map(from_rig_tool_result_content)
            .collect(),
    }
}

pub(crate) fn from_rig_tool_result_content(
    content: &rig::completion::message::ToolResultContent,
) -> message::ToolResultContent {
    match content {
        rig::completion::message::ToolResultContent::Text(text) => {
            message::ToolResultContent::Text(message::Text {
                text: text.text.clone(),
            })
        }
        rig::completion::message::ToolResultContent::Image(image) => {
            message::ToolResultContent::Image(from_rig_image(image))
        }
    }
}

fn from_rig_image(image: &rig::completion::message::Image) -> message::Image {
    message::Image {
        data: match &image.data {
            rig::completion::message::DocumentSourceKind::Url(url) => {
                message::DocumentSourceKind::Url(url.clone())
            }
            rig::completion::message::DocumentSourceKind::Base64(data) => {
                message::DocumentSourceKind::Base64(data.clone())
            }
            rig::completion::message::DocumentSourceKind::Raw(bytes) => {
                message::DocumentSourceKind::Raw(bytes.clone())
            }
            rig::completion::message::DocumentSourceKind::String(text) => {
                message::DocumentSourceKind::String(text.clone())
            }
            _ => message::DocumentSourceKind::Unknown,
        },
        media_type: image.media_type.as_ref().map(|m| {
            use message::ImageMediaType as N;
            match m {
                rig::completion::message::ImageMediaType::JPEG => N::JPEG,
                rig::completion::message::ImageMediaType::PNG => N::PNG,
                rig::completion::message::ImageMediaType::GIF => N::GIF,
                rig::completion::message::ImageMediaType::WEBP => N::WEBP,
                rig::completion::message::ImageMediaType::HEIC => N::HEIC,
                rig::completion::message::ImageMediaType::HEIF => N::HEIF,
                rig::completion::message::ImageMediaType::SVG => N::SVG,
            }
        }),
        detail: image.detail.as_ref().map(|d| match d {
            rig::completion::message::ImageDetail::Low => message::ImageDetail::Low,
            rig::completion::message::ImageDetail::High => message::ImageDetail::High,
            rig::completion::message::ImageDetail::Auto => message::ImageDetail::Auto,
        }),
        additional_params: image.additional_params.clone(),
    }
}

/// Inbound full-message conversion (used by in-crate tests that capture wire
/// requests, and by any future consume-side seam that receives whole rig
/// messages).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn from_rig_message(msg: &rig::completion::Message) -> message::Message {
    match msg {
        rig::completion::Message::System { content } => message::Message::System {
            content: content.clone(),
        },
        rig::completion::Message::User { content } => message::Message::User {
            content: content.iter().map(from_rig_user_content).collect(),
        },
        rig::completion::Message::Assistant { id, content } => message::Message::Assistant {
            id: id.clone(),
            content: content.iter().map(from_rig_assistant_content).collect(),
        },
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn from_rig_user_content(
    content: &rig::completion::message::UserContent,
) -> message::UserContent {
    use rig::completion::message::UserContent as R;
    match content {
        R::Text(text) => message::UserContent::Text(message::Text {
            text: text.text.clone(),
        }),
        R::ToolResult(result) => message::UserContent::ToolResult(from_rig_tool_result(result)),
        R::Image(image) => message::UserContent::Image(from_rig_image(image)),
        // Audio/Video/Document inbound conversions are lossy-stubbed: nothing
        // upstream produces them on the consume seam today, and the native
        // variants exist for outbound fidelity. Extend when a provider sends
        // them.
        R::Audio(_) => {
            tracing::warn!("audio content discarded at the inbound rig seam (lossy stub)");
            message::UserContent::Audio(message::Audio::default())
        }
        R::Video(_) => {
            tracing::warn!("video content discarded at the inbound rig seam (lossy stub)");
            message::UserContent::Video(message::Video::default())
        }
        R::Document(_) => {
            tracing::warn!("document content discarded at the inbound rig seam (lossy stub)");
            message::UserContent::Document(message::Document::default())
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn from_rig_assistant_content(
    content: &rig::completion::message::AssistantContent,
) -> message::AssistantContent {
    use rig::completion::message::AssistantContent as R;
    match content {
        R::Text(text) => message::AssistantContent::Text(message::Text {
            text: text.text.clone(),
        }),
        R::ToolCall(call) => message::AssistantContent::ToolCall(from_rig_tool_call(call)),
        R::Reasoning(reasoning) => {
            message::AssistantContent::Reasoning(from_rig_reasoning(reasoning))
        }
        R::Image(image) => message::AssistantContent::Image(from_rig_image(image)),
    }
}

#[cfg(test)]
mod tests {
    use rig::completion::message::{Message, Text, ToolChoice, UserContent};
    use rig::completion::{CompletionRequest, ToolDefinition};
    use rig::one_or_many::OneOrMany;
    use serde_json::{json, Value};

    use super::*;

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![
                Message::system("You are exact."),
                Message::User {
                    content: OneOrMany::one(UserContent::Text(Text {
                        text: "Read the file.".to_string(),
                    })),
                },
            ])
            .expect("non-empty history"),
            documents: Vec::new(),
            tools: vec![ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            }],
            temperature: Some(0.2),
            max_tokens: Some(512),
            tool_choice: Some(ToolChoice::Auto),
            additional_params: Some(json!({
                "reasoning": { "effort": "medium" }
            })),
            output_schema: None,
        }
    }

    fn sample_request_with_assistant_turn() -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![
                Message::system("You are exact."),
                Message::User {
                    content: OneOrMany::one(UserContent::Text(Text {
                        text: "Read the file.".to_string(),
                    })),
                },
                Message::Assistant {
                    id: None,
                    content: OneOrMany::one(rig::completion::message::AssistantContent::text(
                        "Prior answer.",
                    )),
                },
                Message::User {
                    content: OneOrMany::one(UserContent::Text(Text {
                        text: "Continue.".to_string(),
                    })),
                },
            ])
            .expect("non-empty history"),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: Some(0.2),
            max_tokens: Some(512),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        }
    }

    fn responses_context(
        normalize_responses_wire: bool,
    ) -> crate::rendered_request::RenderedRequestContext {
        crate::rendered_request::RenderedRequestContext {
            request_id: "req-1".to_string(),
            agent_did: "did:key:test".to_string(),
            behavior_id: "behavior".to_string(),
            session_id: "session".to_string(),
            model_name: "test-model".to_string(),
            source: crate::rendered_request::RenderedRequestSource::OpenAiResponses,
            normalize_responses_wire,
        }
    }

    /// TA-2 (#566 review): the rendered-request capture must match the bytes the
    /// transport actually sends. The capture path normalizes the Responses body only
    /// when `normalize_responses_wire` is set (the same gate the outbound
    /// `ResponsesNormalizingHttpClient` uses), so a prior-assistant turn gains the
    /// vLLM-required `id` / `type` / `status` / `output_text.annotations: []`. With the
    /// gate off the capture stays the raw rig shape. This drives the `OpenAiResponses`
    /// branch + the gating boolean that no other test exercises.
    #[test]
    fn responses_capture_normalizes_prior_assistant_items_only_when_enabled() {
        let request = sample_request_with_assistant_turn();
        let rendered_on = rendered_completion_request(&responses_context(true), 0, 0, &request)
            .expect("render normalized");
        let rendered_off = rendered_completion_request(&responses_context(false), 0, 0, &request)
            .expect("render raw");

        // The gating boolean must have an observable effect on the captured body.
        assert_ne!(
            rendered_on.request_json, rendered_off.request_json,
            "normalize_responses_wire must change the captured Responses body"
        );

        let input = rendered_on.request_json["input"]
            .as_array()
            .expect("responses input array");
        let assistant = input
            .iter()
            .find(|item| item["role"] == "assistant")
            .expect("a prior assistant item in the Responses input");
        assert_eq!(assistant["type"], "message");
        assert!(assistant["id"].is_string());
        assert_eq!(assistant["status"], "completed");
        let annotated = assistant["content"]
            .as_array()
            .expect("assistant content array")
            .iter()
            .any(|c| c["type"] == "output_text" && c["annotations"] == json!([]));
        assert!(
            annotated,
            "output_text items must carry annotations: [] after normalization"
        );

        // Without the gate, the raw rig shape carries no added annotations.
        let assistant_off = rendered_off.request_json["input"]
            .as_array()
            .expect("input array")
            .iter()
            .find(|item| item["role"] == "assistant")
            .expect("assistant item (gate off)");
        let any_annotations = assistant_off["content"]
            .as_array()
            .map(|content| content.iter().any(|c| c.get("annotations").is_some()))
            .unwrap_or(false);
        assert!(
            !any_annotations,
            "without normalization the captured body must not carry output_text annotations"
        );
    }

    #[test]
    fn responses_capture_preserves_optional_tool_parameters() {
        let mut request = sample_request();
        request.tools[0].parameters = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "limit": { "type": "integer" }
            },
            "required": ["path"]
        });

        let rendered = rendered_completion_request(&responses_context(false), 0, 0, &request)
            .expect("render Responses request");
        let tool = &rendered.tools_json[0];

        assert_eq!(tool["parameters"]["required"], json!(["path"]));
        assert_ne!(tool.get("strict"), Some(&Value::Bool(true)));
    }

    // ===== #589/#590: argument-shape normalization at both converter seams =====

    fn native_tool_call(arguments: Value) -> message::ToolCall {
        message::ToolCall {
            id: "call-1".to_string(),
            call_id: Some("call-1".to_string()),
            function: message::ToolFunction {
                name: "describe_tool".to_string(),
                arguments,
            },
            signature: None,
            additional_params: None,
        }
    }

    fn rig_tool_call(arguments: Value) -> rig::completion::message::ToolCall {
        rig::completion::message::ToolCall {
            id: "call-1".to_string(),
            call_id: Some("call-1".to_string()),
            function: rig::completion::message::ToolFunction {
                name: "describe_tool".to_string(),
                arguments,
            },
            signature: None,
            additional_params: None,
        }
    }

    /// Ingest seam (#589): nothing non-object is ever accumulated into durable
    /// history. A wire `"[]"` (rig parses it to `Value::Array`) and a raw
    /// corrupt string both normalize; a healthy object is untouched.
    #[test]
    fn from_rig_tool_call_normalizes_arguments_to_object_shape() {
        let object = json!({"city": "NYC"});
        assert_eq!(
            from_rig_tool_call(&rig_tool_call(object.clone()))
                .function
                .arguments,
            object,
            "object arguments must pass through unchanged"
        );

        assert_eq!(
            from_rig_tool_call(&rig_tool_call(json!([])))
                .function
                .arguments,
            json!({}),
            "a non-object array must never be persisted"
        );

        let salvaged = from_rig_tool_call(&rig_tool_call(Value::String(
            crate::test_support::CORRUPT_TOOL_ARGS_589.into(),
        )))
        .function
        .arguments;
        assert!(
            salvaged.is_object(),
            "the #589 corrupt raw string must not reach history as a string"
        );
        assert_eq!(salvaged["tool_name"], "list_hosts");
    }

    /// Egress seam (#590): already-poisoned durable history self-heals at
    /// request build — the provider can never receive a non-object
    /// `arguments`, so the deterministic template-render jam clears on the
    /// next turn without a DB edit.
    #[test]
    fn to_rig_tool_call_normalizes_persisted_poison_on_egress() {
        let object = json!({"city": "NYC"});
        assert_eq!(
            to_rig_tool_call(&native_tool_call(object.clone()))
                .function
                .arguments,
            object,
            "object arguments must pass through unchanged"
        );

        assert_eq!(
            to_rig_tool_call(&native_tool_call(json!([])))
                .function
                .arguments,
            json!({}),
            "a persisted [] must egress as {{}}"
        );
        assert_eq!(
            to_rig_tool_call(&native_tool_call(Value::Null))
                .function
                .arguments,
            json!({}),
            "persisted null args must egress as {{}}"
        );

        // Amy's actual poisoned row: a Value::String of corrupt bytes.
        let healed = to_rig_tool_call(&native_tool_call(Value::String(
            crate::test_support::CORRUPT_TOOL_ARGS_589.into(),
        )))
        .function
        .arguments;
        assert!(
            healed.is_object(),
            "the persisted #589 poison must egress object-shaped"
        );
        assert_eq!(healed["tool_name"], "list_hosts");
    }

    #[test]
    fn renders_openai_chat_provider_shape_for_capture() {
        let context = crate::rendered_request::RenderedRequestContext {
            request_id: "req-1".to_string(),
            agent_did: "did:key:test".to_string(),
            behavior_id: "behavior".to_string(),
            session_id: "session".to_string(),
            model_name: "test-model".to_string(),
            source: crate::rendered_request::RenderedRequestSource::OpenAiChatCompletions,
            normalize_responses_wire: false,
        };

        let rendered =
            rendered_completion_request(&context, 0, 0, &sample_request()).expect("render request");

        assert_eq!(rendered.request_id, "req-1");
        assert_eq!(rendered.turn_index, 0);
        assert_eq!(rendered.attempt, 0);
        assert_eq!(
            rendered.source,
            crate::rendered_request::RenderedRequestSource::OpenAiChatCompletions
        );
        assert_eq!(rendered.messages_json[0]["role"], "system");
        assert_eq!(rendered.messages_json[1]["role"], "user");
        assert_eq!(rendered.tools_json[0]["function"]["name"], "read_file");
        assert_eq!(rendered.sampling_json["temperature"], 0.2);
        assert_eq!(rendered.sampling_json["max_tokens"], 512);
        assert_ne!(rendered.request_json, Value::Null);
        assert_eq!(rendered.prompt_hash.len(), 64);
        assert_eq!(rendered.tools_hash.len(), 64);
    }
}
