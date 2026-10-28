use anyhow::{Context, Result};
use rig::completion::CompletionRequest;
use rig::one_or_many::OneOrMany;
#[cfg(feature = "native")]
use serde_json::Map;
use serde_json::Value;

use crate::backend_provider::BackendProviderKind;
use crate::openai_wire::OpenAiWireApi;

#[path = "provider_input_budget.rs"]
pub mod budget;

pub mod replay_frontier;
pub mod routing_affinity;
mod tool_call_identity;

pub(crate) use tool_call_identity::normalize_tool_call_ids;

#[cfg(all(test, feature = "native"))]
pub(crate) mod tool_call_identity_tests;

/// Native capture metadata belongs to immutable documents. Convert each typed
/// snapshot to provider text before request accounting and durable HTTP capture;
/// never send the storage marker as an opaque provider parameter.
pub fn captured_context_for_provider(
    mut message: gents_protocol::message::Message,
) -> Result<gents_protocol::message::Message> {
    use gents_protocol::message::{Message, UserContent};
    use gents_protocol::request_input::CapturedContext;
    let attached = gents_protocol::request_input::RequestAttachment::from_message(&message);
    let tagged = match &message {
        Message::User { content } => content
            .iter()
            .filter(|part| {
                let params = match part {
                    UserContent::Image(i) => &i.additional_params,
                    UserContent::Document(d) => &d.additional_params,
                    _ => return false,
                };
                params
                    .as_ref()
                    .is_some_and(|p| p.get("gents_attachment").is_some())
            })
            .count(),
        _ => 0,
    };
    anyhow::ensure!(
        tagged == attached.len(),
        "Invalid attachment representation in the transcript"
    );
    gents_protocol::request_input::RequestInput {
        attachments: attached,
        ..Default::default()
    }
    .validate_attachments()?;
    if let Message::User { content } = &mut message {
        for part in content {
            if let UserContent::Document(document) = part {
                if document.media_type == Some(gents_protocol::message::DocumentMediaType::TXT)
                    && document
                        .additional_params
                        .as_ref()
                        .is_some_and(|p| p.get("gents_attachment").is_some())
                {
                    use base64::Engine;
                    let gents_protocol::message::DocumentSourceKind::Base64(data) = &document.data
                    else {
                        anyhow::bail!("Text attachment is not base64 encoded");
                    };
                    let name = document
                        .additional_params
                        .as_ref()
                        .and_then(|p| p.get("gents_attachment"))
                        .and_then(serde_json::Value::as_str)
                        .context("Text attachment has no name")?;
                    let text =
                        String::from_utf8(base64::engine::general_purpose::STANDARD.decode(data)?)?;
                    *part = UserContent::text(format!(
                        "Attached file {}:\n\n{text}",
                        serde_json::to_string(name)?
                    ));
                    continue;
                }
            }
            let params = match part {
                UserContent::Image(i) => &mut i.additional_params,
                UserContent::Document(d) => &mut d.additional_params,
                _ => continue,
            };
            if let Some(serde_json::Value::Object(object)) = params {
                object.remove("gents_attachment");
                if object.is_empty() {
                    *params = None;
                }
            }

            if let UserContent::Document(document) = part {
                if let Some(context) = CapturedContext::from_document(document)? {
                    *part = UserContent::text(format!(
                        "Editor context (captured text; URI identifies its editor location):\n{}",
                        serde_json::to_string(&context)?,
                    ));
                }
            }
        }
    }
    Ok(message)
}

/// The chat-completions DTO flattens documents to prose, so binary PDFs must
/// fail before that conversion rather than becoming base64 text for the model.
pub fn chat_completions_attachment_supported(media_type: &str) -> bool {
    media_type != "application/pdf"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ServiceTier {
    Default,
    Fast,
}

impl ServiceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Fast => "fast",
        }
    }

    pub fn wire_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Fast => "priority",
        }
    }

    /// Rig's Responses DTO accepts fewer tiers than the provider. Apply the
    /// native selection after DTO conversion, before accounting and capture.
    pub fn apply(self, body: &mut Value) -> Result<()> {
        body.as_object_mut()
            .context("provider request must be an object")?
            .insert("service_tier".into(), self.wire_value().into());
        Ok(())
    }

    pub fn patch(self, body: &[u8]) -> Result<Vec<u8>> {
        let mut value = serde_json::from_slice(body).context("decoding provider request")?;
        self.apply(&mut value)?;
        serde_json::to_vec(&value).context("encoding provider request")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderInputProfile {
    OpenAiChatCompletions,
    OpenAiResponsesNormalized,
    OpenRouterChatCompletions,
    ChatGptCodexResponses,
    XaiResponses,
    ClaudeMessages,
}

impl ProviderInputProfile {
    pub fn carries_tool_result_images(self) -> bool {
        self == Self::ClaudeMessages
    }

    pub fn replay_wire(self) -> Option<crate::claude_messages_body::ReplayWire> {
        use crate::claude_messages_body::ReplayWire;
        match self {
            Self::ClaudeMessages => Some(ReplayWire::ClaudeMessages),
            Self::OpenAiResponsesNormalized | Self::ChatGptCodexResponses | Self::XaiResponses => {
                Some(ReplayWire::Responses)
            }
            Self::OpenAiChatCompletions | Self::OpenRouterChatCompletions => None,
        }
    }

    pub fn resolve(provider: BackendProviderKind, wire: OpenAiWireApi) -> Self {
        match (provider, wire) {
            (BackendProviderKind::OpenAiCompatible, OpenAiWireApi::ChatCompletions)
            | (BackendProviderKind::XaiGrokOAuth, OpenAiWireApi::ChatCompletions) => {
                Self::OpenAiChatCompletions
            }
            (BackendProviderKind::OpenAiCompatible, OpenAiWireApi::Responses) => {
                Self::OpenAiResponsesNormalized
            }
            (BackendProviderKind::OpenRouter, _) => Self::OpenRouterChatCompletions,
            (BackendProviderKind::ChatGptCodex, _) => Self::ChatGptCodexResponses,
            (BackendProviderKind::XaiGrokOAuth, OpenAiWireApi::Responses) => Self::XaiResponses,
            (BackendProviderKind::ClaudeCliSubscription, _)
            | (BackendProviderKind::AnthropicApiKey, _) => Self::ClaudeMessages,
        }
    }

    const fn estimator_name(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai_chat_wire_json_bytes_div_4_v1",
            Self::OpenAiResponsesNormalized => "openai_responses_wire_json_bytes_div_4_v1",
            Self::OpenRouterChatCompletions => "openrouter_chat_wire_json_bytes_div_4_v1",
            Self::ChatGptCodexResponses => "chatgpt_codex_wire_json_bytes_div_4_v1",
            Self::XaiResponses => "xai_responses_wire_json_bytes_div_4_v1",
            Self::ClaudeMessages => "claude_messages_wire_json_bytes_div_4_v1",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderInputCounter {
    profile: ProviderInputProfile,
    model: String,
    service_tier: Option<ServiceTier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInputProjection {
    pub components: gents_protocol::rendered_request::ContextInputComponents,
    pub estimated_input_tokens: usize,
    pub estimator: &'static str,
}

impl ProviderInputCounter {
    pub fn new(
        provider: BackendProviderKind,
        wire: OpenAiWireApi,
        model: impl Into<String>,
    ) -> Self {
        Self {
            profile: ProviderInputProfile::resolve(provider, wire),
            model: model.into(),
            service_tier: None,
        }
    }

    pub fn with_service_tier(mut self, service_tier: Option<ServiceTier>) -> Self {
        self.service_tier = service_tier;
        self
    }

    pub fn profile(&self) -> ProviderInputProfile {
        self.profile
    }

    pub fn project_request(&self, request: &CompletionRequest) -> Result<ProviderInputProjection> {
        let body = self.estimated_body(request)?;
        let documentless_body = if request.documents.is_empty() {
            None
        } else {
            let mut documentless = request.clone();
            documentless.documents.clear();
            Some(self.estimated_body(&documentless)?)
        };

        projected_accounting(body, documentless_body, self.profile.estimator_name())
    }

    pub fn estimate_request(&self, request: &CompletionRequest) -> Result<usize> {
        estimate_input_body(self.estimated_body(request)?)
    }

    fn estimated_body(&self, request: &CompletionRequest) -> Result<Value> {
        let mut body = self.project_body(request)?;
        if self.profile == ProviderInputProfile::ClaudeMessages {
            if let Some(messages) = body.get_mut("messages") {
                charge_claude_images(messages);
            }
        }
        Ok(body)
    }

    #[cfg(feature = "native")]
    pub fn project_body(&self, request: &CompletionRequest) -> Result<Value> {
        let mut body = match self.profile {
            ProviderInputProfile::OpenAiChatCompletions => {
                use rig::message::MimeType;
                for message in request.chat_history.iter() {
                    if let rig::completion::Message::User { content } = message {
                        for part in content.iter() {
                            if let rig::message::UserContent::Document(document) = part {
                                anyhow::ensure!(document.media_type.as_ref().is_none_or(|media| chat_completions_attachment_supported(media.to_mime_type())), "PDF attachments are not supported by the OpenAI Chat Completions transport; use a provider with document support");
                            }
                        }
                    }
                }
                let dto = rig::providers::openai::completion::CompletionRequest::try_from((
                    self.model.clone(),
                    request.clone(),
                ))
                .context("projecting OpenAI Chat Completions request")?;
                let mut body = serde_json::to_value(dto)
                    .context("serializing OpenAI Chat Completions request")?;
                set_streaming_fields(&mut body, true);
                body
            }
            ProviderInputProfile::OpenAiResponsesNormalized => {
                let mut body = self.responses_body(request)?;
                set_streaming_fields(&mut body, false);
                crate::responses_normalize::normalize_responses_assistant_items(&mut body);
                body
            }
            ProviderInputProfile::OpenRouterChatCompletions => {
                let mut body = self.openrouter_body(request)?;
                set_streaming_fields(&mut body, false);
                body
            }
            ProviderInputProfile::ChatGptCodexResponses => {
                let mut body = self.responses_body(request)?;
                set_streaming_fields(&mut body, false);
                rewrite_bytes(body, crate::provider_patches::patch_instructions_body)?
            }
            ProviderInputProfile::XaiResponses => {
                let mut body = self.responses_body(request)?;
                set_streaming_fields(&mut body, false);
                rewrite_bytes(body, crate::provider_patches::patch_store_false)?
            }
            ProviderInputProfile::ClaudeMessages => {
                crate::claude_messages_body::build_messages_body(&self.model, request)?
            }
        };
        if let Some(tier) = self.service_tier {
            tier.apply(&mut body)?;
        }
        Ok(body)
    }

    #[cfg(not(feature = "native"))]
    /// Guest fallback serializes Rig core requests and undercounts wire input.
    pub fn project_body(&self, request: &CompletionRequest) -> Result<Value> {
        if self.profile == ProviderInputProfile::ClaudeMessages {
            return crate::claude_messages_body::build_messages_body(&self.model, request);
        }
        let mut body = serde_json::json!({
            "model": request.model.clone().unwrap_or_else(|| self.model.clone()),
            "preamble": request.preamble,
            "chat_history": request.chat_history,
            "tools": request.tools,
            "tool_choice": request.tool_choice,
            "temperature": request.temperature,
            "additional_params": request.additional_params,
        });
        set_streaming_fields(&mut body, true);
        Ok(body)
    }

    pub fn estimate_message_request(
        &self,
        messages: &[gents_protocol::message::Message],
    ) -> Result<usize> {
        if messages.is_empty() {
            return Ok(0);
        }
        #[cfg(feature = "native")]
        if self.profile == ProviderInputProfile::OpenAiChatCompletions {
            let has_visible_message = crate::rig_compat::to_rig_messages(messages)
                .into_iter()
                .map(Vec::<rig::providers::openai::completion::Message>::try_from)
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .any(|converted| !converted.is_empty());
            if !has_visible_message {
                return Ok(0);
            }
        }
        let chat_history = OneOrMany::many(crate::rig_compat::to_rig_messages(messages))
            .map_err(|_| anyhow::anyhow!("provider input message projection was empty"))?;
        let request = CompletionRequest {
            model: None,
            preamble: None,
            chat_history,
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };
        self.estimate_request(&request)
    }

    #[cfg(feature = "native")]
    fn responses_body(&self, request: &CompletionRequest) -> Result<Value> {
        let mut request = request.clone();
        for message in request.chat_history.iter_mut() {
            normalize_tool_call_ids(self.profile, message);
        }
        let dto = rig::providers::openai::responses_api::CompletionRequest::try_from((
            self.model.clone(),
            request,
        ))
        .context("projecting OpenAI Responses request")?;
        let mut body = serde_json::to_value(dto).context("serializing OpenAI Responses request")?;
        if self.profile != ProviderInputProfile::XaiResponses {
            crate::responses_normalize::normalize_function_call_item_ids(&mut body);
        }
        Ok(body)
    }

    #[cfg(feature = "native")]
    fn openrouter_body(&self, request: &CompletionRequest) -> Result<Value> {
        use rig::providers::openrouter::completion::Message as OpenRouterMessage;

        let mut messages = Vec::new();
        if let Some(preamble) = request.preamble.as_ref() {
            messages.extend(
                Vec::<OpenRouterMessage>::try_from(rig::completion::Message::system(preamble))
                    .context("projecting OpenRouter preamble")?,
            );
        }
        if let Some(documents) = request.normalized_documents() {
            messages.extend(
                Vec::<OpenRouterMessage>::try_from(documents)
                    .context("projecting OpenRouter documents")?,
            );
        }
        for message in request.chat_history.clone() {
            messages.extend(
                Vec::<OpenRouterMessage>::try_from(message)
                    .context("projecting OpenRouter message")?,
            );
        }

        let tools = request
            .tools
            .clone()
            .into_iter()
            .map(rig::providers::openai::completion::ToolDefinition::from)
            .collect::<Vec<_>>();
        let tool_choice = request
            .tool_choice
            .clone()
            .map(rig::providers::openai::completion::ToolChoice::try_from)
            .transpose()
            .context("projecting OpenRouter tool choice")?;

        let mut body = Map::new();
        body.insert(
            "model".to_string(),
            Value::String(request.model.clone().unwrap_or_else(|| self.model.clone())),
        );
        body.insert("messages".to_string(), serde_json::to_value(messages)?);
        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_string(), Value::from(temperature));
        }
        if !tools.is_empty() {
            body.insert("tools".to_string(), serde_json::to_value(tools)?);
        }
        if let Some(tool_choice) = tool_choice {
            body.insert(
                "tool_choice".to_string(),
                serde_json::to_value(tool_choice)?,
            );
        }
        if let Some(additional) = request
            .additional_params
            .as_ref()
            .and_then(Value::as_object)
        {
            for (key, value) in additional {
                body.insert(key.clone(), value.clone());
            }
        }
        Ok(Value::Object(body))
    }
}

/// Conservative ceiling: undercounting could admit a request over provider context.
pub const CLAUDE_IMAGE_TOKENS: usize = 4_784;

fn charge_claude_images(value: &mut Value) {
    match value {
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("image") => {
            if let Some(source) = map.get_mut("source") {
                *source = Value::String("x".repeat(CLAUDE_IMAGE_TOKENS * 4));
            }
        }
        Value::Object(map) => map.values_mut().for_each(charge_claude_images),
        Value::Array(items) => items.iter_mut().for_each(charge_claude_images),
        _ => {}
    }
}

pub fn set_streaming_fields(body: &mut Value, include_usage: bool) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.insert("stream".to_string(), Value::Bool(true));
    if include_usage {
        object.insert(
            "stream_options".to_string(),
            serde_json::json!({"include_usage": true}),
        );
    }
}

#[cfg(feature = "native")]
fn rewrite_bytes(body: Value, rewrite: fn(&[u8]) -> Option<bytes::Bytes>) -> Result<Value> {
    let encoded = serde_json::to_vec(&body).context("serializing provider body for rewrite")?;
    match rewrite(&encoded) {
        Some(rewritten) => serde_json::from_slice(&rewritten)
            .context("decoding deterministically rewritten provider body"),
        None => Ok(body),
    }
}

fn projected_accounting(
    mut body: Value,
    mut documentless_body: Option<Value>,
    estimator: &'static str,
) -> Result<ProviderInputProjection> {
    remove_output_limits(&mut body);

    let provider_messages =
        field_estimate(&body, &["messages", "system", "input", "instructions"])?;
    let documentless_messages = documentless_body
        .as_mut()
        .map(|body| {
            remove_output_limits(body);
            field_estimate(body, &["messages", "system", "input", "instructions"])
        })
        .transpose()?
        .unwrap_or(provider_messages);
    let documents = provider_messages
        .checked_sub(documentless_messages)
        .context("provider document projection reduced the provider message estimate")?;
    let messages = documentless_messages;
    let tool_schemas = field_estimate(&body, &["tools", "tool_choice"])?;
    let claude_schema = body
        .get("output_config")
        .and_then(|config| config.get("format"))
        .map(estimate_json)
        .transpose()?
        .unwrap_or(0);
    let output_schema = field_estimate(&body, &["response_format", "text"])?
        .checked_add(claude_schema)
        .context("provider output schema estimate overflow")?;
    let estimated_input_tokens = estimate_input_body(body)?;
    let classified = messages
        .checked_add(documents)
        .and_then(|total| total.checked_add(tool_schemas))
        .and_then(|total| total.checked_add(output_schema))
        .context("provider input component estimate overflow")?;
    let additional_parameters = estimated_input_tokens.checked_sub(classified).ok_or_else(|| {
        anyhow::anyhow!(
            "provider input component partition exceeded total: classified={classified}, total={estimated_input_tokens}"
        )
    })?;
    let components = gents_protocol::rendered_request::ContextInputComponents {
        messages,
        documents,
        tool_schemas,
        additional_parameters,
        output_schema,
    };

    Ok(ProviderInputProjection {
        components,
        estimated_input_tokens,
        estimator,
    })
}

pub fn estimate_input_body(mut body: Value) -> Result<usize> {
    remove_output_limits(&mut body);
    estimate_json(&body)
}

fn remove_output_limits(body: &mut Value) {
    if let Some(object) = body.as_object_mut() {
        object.remove("max_tokens");
        object.remove("max_output_tokens");
    }
}

fn field_estimate(value: &Value, fields: &[&str]) -> Result<usize> {
    let Some(object) = value.as_object() else {
        return Ok(0);
    };
    fields.iter().try_fold(0usize, |total, field| {
        let field_tokens = object
            .get(*field)
            .map(estimate_json)
            .transpose()?
            .unwrap_or(0);
        total
            .checked_add(field_tokens)
            .context("provider input field estimate overflow")
    })
}

pub fn estimate_json(value: &Value) -> Result<usize> {
    Ok(serde_json::to_vec(value)
        .context("serializing provider input for token estimate")?
        .len()
        / 4)
}
