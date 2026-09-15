//! Provider-shaped preflight accounting.
//!
//! Rig's core [`rig::completion::CompletionRequest`] is an intermediate. Each
//! backend converts it again before sending, and those conversions are
//! semantically significant: OpenAI Chat Completions, for example, omits
//! assistant-history reasoning entirely. Budget decisions must therefore use
//! the same selected projection as the provider client, not serialization of
//! the core request.

use anyhow::{Context, Result};
use rig::completion::CompletionRequest;
#[cfg(feature = "native")]
use serde_json::Map;
use serde_json::Value;

use crate::backend_provider::BackendProviderKind;
use crate::openai_wire::OpenAiWireApi;

#[path = "provider_input_budget.rs"]
pub mod budget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderInputProfile {
    OpenAiChatCompletions,
    OpenAiResponsesNormalized,
    OpenRouterChatCompletions,
    ChatGptCodexResponses,
    XaiResponses,
    /// Anthropic Messages body (`claude_messages`); the OpenAI wire setting
    /// does not apply.
    ClaudeMessages,
}

impl ProviderInputProfile {
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
            (BackendProviderKind::ClaudeCliSubscription, _) => Self::ClaudeMessages,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInputProjection {
    pub components: gents_protocol::rendered_request::ContextInputComponents,
    /// One authoritative estimate over the composed provider projection. It is
    /// deliberately not a sum of independently floored component estimates.
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
        }
    }

    // Not `#[cfg(test)]`: gents' own provider_input tests read the resolved
    // profile, and a cfg(test) item in this crate is invisible to a
    // dependent crate's own test build.
    pub fn profile(&self) -> ProviderInputProfile {
        self.profile
    }

    pub fn project_request(&self, request: &CompletionRequest) -> Result<ProviderInputProjection> {
        let body = self.project_body(request)?;
        let documentless_body = if request.documents.is_empty() {
            None
        } else {
            let mut documentless = request.clone();
            documentless.documents.clear();
            Some(self.project_body(&documentless)?)
        };

        projected_accounting(body, documentless_body, self.profile.estimator_name())
    }

    /// Estimate one complete provider request without computing the diagnostic
    /// component partition. Candidate search and admission use this scalar hot
    /// path; rendered-request accounting calls `project_request` once for the
    /// request that may actually be dispatched.
    pub fn estimate_request(&self, request: &CompletionRequest) -> Result<usize> {
        let mut body = self.project_body(request)?;
        remove_output_limits(&mut body);
        estimate_json(&body)
    }

    // pub, not private: gents' own provider_input tests project a request
    // body directly to assert its exact shape.
    #[cfg(feature = "native")]
    pub fn project_body(&self, request: &CompletionRequest) -> Result<Value> {
        let body = match self.profile {
            ProviderInputProfile::OpenAiChatCompletions => {
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
                crate::claude_messages_body::build_messages_body(&self.model, request)
            }
        };
        Ok(body)
    }

    /// Guest fallback (no `native` feature, so no `rig::providers::*` DTOs):
    /// serialize the core `CompletionRequest` directly. This undercounts
    /// relative to the wire-exact projection above (no provider-specific
    /// framing overhead), so it is honest, not precise - the guest has no
    /// provider client to be byte-exact against in the first place.
    /// vertexia: ceiling is wire-exact accounting once rig's `providers`
    /// feature gate splits DTOs from the network client (upstream), or once
    /// the host-import transport (Phase 3b) supplies the wire body directly.
    #[cfg(not(feature = "native"))]
    pub fn project_body(&self, request: &CompletionRequest) -> Result<Value> {
        if self.profile == ProviderInputProfile::ClaudeMessages {
            return Ok(crate::claude_messages_body::build_messages_body(
                &self.model,
                request,
            ));
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

    /// Estimate a messages-only provider request. The result includes the
    /// selected wire API's request framing; it is not an additive per-row cost.
    pub fn estimate_message_request(
        &self,
        messages: &[gents_protocol::message::Message],
    ) -> Result<usize> {
        if messages.is_empty() {
            return Ok(0);
        }
        #[cfg(feature = "native")]
        if self.profile == ProviderInputProfile::OpenAiChatCompletions {
            // OpenAI Chat Completions drops some native content (e.g. bare
            // reasoning) entirely; skip the provider call when nothing would
            // actually render. Native-only: needs the provider DTO to know.
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
        let chat_history =
            rig::one_or_many::OneOrMany::many(crate::rig_compat::to_rig_messages(messages))
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
        let dto = rig::providers::openai::responses_api::CompletionRequest::try_from((
            self.model.clone(),
            request.clone(),
        ))
        .context("projecting OpenAI Responses request")?;
        serde_json::to_value(dto).context("serializing OpenAI Responses request")
    }

    /// Rig keeps its complete OpenRouter DTO crate-private. Build the same
    /// shape from Rig's public provider message/tool converters so production
    /// accounting still follows the actual wire representation, including
    /// `reasoning_details`.
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

// pub, not private: gents' own provider_input tests exercise this streaming
// field patch directly, and a private item in this crate is invisible outside
// it.
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
    let estimated_input_tokens = estimate_json(&body)?;

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
    let output_schema = field_estimate(&body, &["response_format", "text"])?;
    let classified = messages
        .checked_add(documents)
        .and_then(|total| total.checked_add(tool_schemas))
        .and_then(|total| total.checked_add(output_schema))
        .context("provider input component estimate overflow")?;
    // Framing, model, and provider parameters occupy the remainder. Assigning
    // the remainder makes the diagnostic partition agree
    // exactly with the one-shot authoritative total despite `/4` remainders.
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

fn remove_output_limits(body: &mut Value) {
    // Output limits are dispatch parameters, not provider input. Excluding
    // them also avoids circular accounting when the dynamic clamp changes the
    // number of digits in the field itself.
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

// The test suite stayed in gents (crates/gents/src/provider_input/tests.rs):
// most of it exercises this module through the real transport clients
// (chatgpt_codex, xai_grok_oauth, inference_http), which are native.
