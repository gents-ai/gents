use serde::{Deserialize, Serialize};

use crate::backend_provider::BackendProviderKind;

#[derive(Debug, thiserror::Error)]
pub enum OpenAiWireApiError {
    #[error("unknown OpenAI wire API {api}")]
    Unknown { api: String },
}

type Result<T, E = OpenAiWireApiError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum OpenAiWireApi {
    #[serde(rename = "responses")]
    Responses,
    #[serde(rename = "chat_completions")]
    ChatCompletions,
}

impl OpenAiWireApi {
    pub const ALL: [Self; 2] = [Self::Responses, Self::ChatCompletions];

    pub fn parse_optional(value: Option<&str>) -> Result<Option<Self>> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(None),
            Some("responses") => Ok(Some(Self::Responses)),
            Some("chat_completions") | Some("chat-completions") => Ok(Some(Self::ChatCompletions)),
            Some(other) => Err(OpenAiWireApiError::Unknown {
                api: other.to_owned(),
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
        }
    }

    pub fn effective_for_provider(
        provider_kind: BackendProviderKind,
        configured: Option<Self>,
    ) -> Self {
        match provider_kind {
            BackendProviderKind::OpenAiCompatible => configured.unwrap_or(Self::ChatCompletions),
            BackendProviderKind::OpenRouter => Self::ChatCompletions,
            BackendProviderKind::ChatGptCodex => Self::Responses,
            BackendProviderKind::XaiGrokOAuth => configured.unwrap_or(Self::Responses),
            BackendProviderKind::ClaudeCliSubscription
            | BackendProviderKind::AnthropicApiKey
            | BackendProviderKind::ClaudeBedrock
            | BackendProviderKind::ClaudeVertex
            | BackendProviderKind::ClaudeFoundry
            | BackendProviderKind::ClaudeAnthropicAws
            | BackendProviderKind::ClaudeMantle
            | BackendProviderKind::ClaudeGoogleCloud
            | BackendProviderKind::ClaudeGateway => Self::ChatCompletions,
        }
    }

    pub fn warn_if_ignored(
        provider_kind: BackendProviderKind,
        configured: Option<Self>,
        backend_id: &str,
    ) {
        let Some(configured) = configured else {
            return;
        };
        if provider_kind.uses_messages_wire()
            || matches!(
                provider_kind,
                BackendProviderKind::OpenRouter | BackendProviderKind::ChatGptCodex
            )
        {
            tracing::warn!(
                backend_id,
                openai_wire_api = configured.as_str(),
                provider_kind = %provider_kind,
                "openai_wire_api is ignored for this backend provider"
            );
        }
    }
}

impl std::fmt::Display for OpenAiWireApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
