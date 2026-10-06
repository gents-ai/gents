use crate::error::{
    classify_loop_failure, CompletionFailure, InferenceError, LoopFailureCause, LoopStreamError,
};
use crate::tool::ToolDefinition;
use crate::ToolChoice;

/// A provider completion model the owned loop drives. Runtime owners are
/// generic over this bound so the provider client's traits stay here.
pub trait ProviderModel:
    rig::completion::CompletionModel<Response: 'static, StreamingResponse: 'static> + 'static
{
}

impl<M> ProviderModel for M
where
    M: rig::completion::CompletionModel + 'static,
    M::Response: 'static,
    M::StreamingResponse: 'static,
{
}

/// A built provider client; `completion_model` names one [`ProviderModel`].
pub trait ProviderClient: rig::client::CompletionClient<CompletionModel: ProviderModel> {}

impl<C> ProviderClient for C
where
    C: rig::client::CompletionClient,
    C::CompletionModel: ProviderModel,
{
}

/// Whether the provider's raw response carries an OpenAI Responses cache-token
/// observation. Rig's `Usage` turns a missing `input_tokens_details` into zero,
/// so admission must consult the raw response before persisting the nullable
/// InferenceCall column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachedInputTokensObservation {
    /// The raw response is not a Responses API payload; retain Rig's behavior.
    NotAvailable,
    /// Responses usage was present but had no cached-token detail.
    Unreported,
    /// The provider explicitly supplied a cached-token count, including zero.
    Reported(u64),
}

/// Read cached-token presence from a serializable raw provider response.
///
/// The Responses final streaming item and non-streaming raw response both carry
/// `usage.output_tokens_details`; this identifies the Responses usage shape
/// without changing other providers' existing accounting behavior.
pub fn cached_input_tokens_observation<T: serde::Serialize>(
    raw_response: &T,
) -> CachedInputTokensObservation {
    let Ok(value) = serde_json::to_value(raw_response) else {
        return CachedInputTokensObservation::NotAvailable;
    };
    let Some(usage) = value.get("usage").and_then(serde_json::Value::as_object) else {
        return CachedInputTokensObservation::NotAvailable;
    };
    if !usage.contains_key("output_tokens_details") {
        return CachedInputTokensObservation::NotAvailable;
    }
    usage
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(serde_json::Value::as_u64)
        .map(CachedInputTokensObservation::Reported)
        .unwrap_or(CachedInputTokensObservation::Unreported)
}

pub fn to_rig_tool_definition(def: &ToolDefinition) -> rig::completion::ToolDefinition {
    rig::completion::ToolDefinition {
        name: def.name.clone(),
        description: def.description.clone(),
        parameters: def.parameters.clone(),
    }
}

pub fn to_rig_tool_choice(choice: &ToolChoice) -> rig::message::ToolChoice {
    match choice {
        ToolChoice::Auto => rig::message::ToolChoice::Auto,
        ToolChoice::None => rig::message::ToolChoice::None,
        ToolChoice::Required => rig::message::ToolChoice::Required,
        ToolChoice::Specific { function_names } => rig::message::ToolChoice::Specific {
            function_names: function_names.clone(),
        },
    }
}

use gents_protocol::message;

pub fn to_rig_messages(messages: &[message::Message]) -> Vec<rig::completion::Message> {
    messages.iter().map(to_rig_message).collect()
}

pub fn to_rig_message(msg: &message::Message) -> rig::completion::Message {
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

pub fn to_rig_user_content(
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

pub fn to_rig_assistant_content(
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

pub fn to_rig_text(text: &message::Text) -> rig::completion::message::Text {
    rig::completion::message::Text {
        text: text.text.clone(),
    }
}

pub fn to_rig_tool_call(call: &message::ToolCall) -> rig::completion::message::ToolCall {
    rig::completion::message::ToolCall {
        id: call.id.clone(),
        call_id: call.call_id.clone(),
        function: rig::completion::message::ToolFunction {
            name: call.function.name.clone(),
            arguments: crate::tool::normalize_tool_call_arguments(
                "egress",
                &call.function.name,
                &call.function.arguments,
            ),
        },
        signature: call.signature.clone(),
        additional_params: call.additional_params.clone(),
    }
}

pub fn to_rig_tool_result(result: &message::ToolResult) -> rig::completion::message::ToolResult {
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

pub fn to_rig_tool_result_content(
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

pub fn to_rig_reasoning(reasoning: &message::Reasoning) -> rig::completion::message::Reasoning {
    let mut rig_reasoning = rig::completion::message::Reasoning::new("");
    rig_reasoning.id = reasoning.id.clone();
    rig_reasoning.content = reasoning
        .content
        .iter()
        .map(to_rig_reasoning_part)
        .collect();
    rig_reasoning
}

pub fn to_rig_reasoning_part(
    part: &message::ReasoningContent,
) -> rig::completion::message::ReasoningContent {
    match part {
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
    }
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

pub fn from_rig_tool_call(call: &rig::completion::message::ToolCall) -> message::ToolCall {
    message::ToolCall {
        id: call.id.clone(),
        call_id: call.call_id.clone(),
        function: message::ToolFunction {
            name: call.function.name.clone(),
            arguments: crate::tool::normalize_tool_call_arguments(
                "ingest",
                &call.function.name,
                &call.function.arguments,
            ),
        },
        signature: call.signature.clone(),
        additional_params: call.additional_params.clone(),
    }
}

pub fn from_rig_reasoning(reasoning: &rig::completion::message::Reasoning) -> message::Reasoning {
    message::Reasoning {
        id: reasoning.id.clone(),
        content: reasoning
            .content
            .iter()
            .map(from_rig_reasoning_part)
            .collect(),
    }
}

pub fn from_rig_reasoning_part(
    part: &rig::completion::message::ReasoningContent,
) -> message::ReasoningContent {
    match part {
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
    }
}

pub fn from_rig_tool_result(result: &rig::completion::message::ToolResult) -> message::ToolResult {
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

pub fn from_rig_tool_result_content(
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

#[cfg_attr(not(test), allow(dead_code))]
pub fn from_rig_message(msg: &rig::completion::Message) -> message::Message {
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
pub fn from_rig_user_content(
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
pub fn from_rig_assistant_content(
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

/// The native cause of a rig stream failure.
pub fn loop_failure_cause(error: &rig::agent::StreamingError) -> LoopFailureCause {
    use rig::completion::{CompletionError, PromptError};
    match error {
        rig::agent::StreamingError::Completion(completion) => LoopFailureCause::Completion {
            failure: match completion {
                CompletionError::HttpError(_) => CompletionFailure::Http,
                CompletionError::ProviderError(message) => {
                    CompletionFailure::Provider(message.clone())
                }
                CompletionError::JsonError(_)
                | CompletionError::UrlError(_)
                | CompletionError::RequestError(_) => CompletionFailure::Request,
                CompletionError::ResponseError(_) => CompletionFailure::Response,
            },
            reason: completion.to_string(),
        },
        rig::agent::StreamingError::Prompt(prompt) => match **prompt {
            PromptError::MaxTurnsError { .. } => LoopFailureCause::MaxTurns,
            _ => LoopFailureCause::Prompt,
        },
        rig::agent::StreamingError::Tool(_) => LoopFailureCause::Tool,
    }
}

/// Carry a rig stream failure across the loop's output boundary.
pub fn loop_stream_error(error: rig::agent::StreamingError) -> LoopStreamError {
    LoopStreamError::new(loop_failure_cause(&error), error)
}

pub fn classify_completion_error(error: &rig::agent::StreamingError) -> InferenceError {
    classify_loop_failure(&loop_failure_cause(error), &error.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    #[cfg(feature = "native")]
    #[test]
    fn responses_wire_conversion_preserves_prompt_cache_key() {
        use rig::providers::openai::responses_api::CompletionRequest as ResponsesCompletionRequest;

        let request = rig::completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: rig::one_or_many::OneOrMany::one(rig::completion::Message::user(
                "Continue this session",
            )),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(128),
            tool_choice: None,
            additional_params: Some(json!({"prompt_cache_key": "session-cache-key"})),
            output_schema: None,
        };
        let converted = ResponsesCompletionRequest::try_from(("gpt-test".to_string(), request))
            .expect("convert Responses request");
        let wire = serde_json::to_value(converted).expect("serialize Responses wire request");
        assert_eq!(wire["prompt_cache_key"], "session-cache-key");
        assert!(wire.get("additional_params").is_none());
    }

    #[cfg(feature = "native")]
    #[test]
    fn responses_cache_observation_preserves_absent_zero_and_positive_details() {
        type Response =
            rig::providers::openai::responses_api::streaming::StreamingCompletionResponse;
        let deserialize = |json| serde_json::from_value::<Response>(json).unwrap();
        let base = json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 120
            }
        });
        let missing = deserialize(base.clone());
        assert_eq!(
            cached_input_tokens_observation(&missing),
            CachedInputTokensObservation::Unreported
        );

        let zero = deserialize(json!({
            "usage": {
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 20,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 120
            }
        }));
        assert_eq!(
            cached_input_tokens_observation(&zero),
            CachedInputTokensObservation::Reported(0)
        );

        let positive = deserialize(json!({
            "usage": {
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 64},
                "output_tokens": 20,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 120
            }
        }));
        assert_eq!(
            cached_input_tokens_observation(&positive),
            CachedInputTokensObservation::Reported(64)
        );

        #[derive(serde::Serialize)]
        struct OtherProvider {
            usage: UsageShape,
        }
        #[derive(serde::Serialize)]
        struct UsageShape {
            input_tokens: u64,
            cached_input_tokens: u64,
        }
        assert_eq!(
            cached_input_tokens_observation(&OtherProvider {
                usage: UsageShape {
                    input_tokens: 100,
                    cached_input_tokens: 7,
                },
            }),
            CachedInputTokensObservation::NotAvailable
        );
    }

    fn max_turns_stream_failure(max_turns: usize) -> rig::agent::StreamingError {
        rig::agent::StreamingError::Prompt(Box::new(rig::completion::PromptError::MaxTurnsError {
            max_turns,
            chat_history: Box::new(Vec::new()),
            prompt: Box::new(rig::completion::Message::user("classify me")),
        }))
    }

    #[test]
    fn turn_exhaustion_classifies_as_max_turns() {
        assert_eq!(
            loop_failure_cause(&max_turns_stream_failure(1_000)),
            LoopFailureCause::MaxTurns
        );
    }

    #[test]
    fn provider_failure_does_not_classify_as_max_turns() {
        let error = rig::agent::StreamingError::Completion(
            rig::completion::CompletionError::ProviderError("boom".to_string()),
        );
        assert_ne!(loop_failure_cause(&error), LoopFailureCause::MaxTurns);
    }

    #[test]
    fn a_provider_failure_echoing_max_turn_wording_still_classifies_as_other() {
        let error = rig::agent::StreamingError::Completion(
            rig::completion::CompletionError::ProviderError(
                "upstream mentioned MaxTurnError: (reached max turn limit: 1000)".to_string(),
            ),
        );
        assert_ne!(loop_failure_cause(&error), LoopFailureCause::MaxTurns);
    }

    #[test]
    fn loop_stream_error_keeps_the_rig_rendering_and_chain() {
        let error = rig::agent::StreamingError::Completion(
            rig::completion::CompletionError::ProviderError("boom".to_string()),
        );
        let expected = format!(
            "{:#}",
            anyhow::Error::new(rig::agent::StreamingError::Completion(
                rig::completion::CompletionError::ProviderError("boom".to_string()),
            ),)
        );
        let native = loop_stream_error(error);
        assert_eq!(native.to_string(), "CompletionError: ProviderError: boom");
        assert_eq!(format!("{:#}", anyhow::Error::new(native)), expected);
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
}
