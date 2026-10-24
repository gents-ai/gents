use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum BackendProviderKindError {
    #[error("backend provider kind is required")]
    Required,
    #[error("unknown backend provider kind {kind}")]
    Unknown { kind: String },
}

type Result<T, E = BackendProviderKindError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum BackendProviderKind {
    #[default]
    #[serde(rename = "OpenAiCompatible")]
    OpenAiCompatible,
    #[serde(rename = "OpenRouter")]
    OpenRouter,
    #[serde(rename = "ChatGptCodex")]
    ChatGptCodex,
    #[serde(rename = "XaiGrokOAuth")]
    XaiGrokOAuth,
    #[serde(rename = "ClaudeCliSubscription")]
    ClaudeCliSubscription,
    #[serde(rename = "AnthropicApiKey")]
    AnthropicApiKey,
}

impl BackendProviderKind {
    pub const ALL: [Self; 6] = [
        Self::OpenAiCompatible,
        Self::OpenRouter,
        Self::ChatGptCodex,
        Self::XaiGrokOAuth,
        Self::ClaudeCliSubscription,
        Self::AnthropicApiKey,
    ];

    pub fn parse_optional(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Err(BackendProviderKindError::Required),
            Some("OpenAiCompatible") => Ok(Self::OpenAiCompatible),
            Some("OpenRouter") => Ok(Self::OpenRouter),
            Some("ChatGptCodex") => Ok(Self::ChatGptCodex),
            Some("XaiGrokOAuth") => Ok(Self::XaiGrokOAuth),
            Some("ClaudeCliSubscription") => Ok(Self::ClaudeCliSubscription),
            Some("AnthropicApiKey") => Ok(Self::AnthropicApiKey),
            Some(other) => Err(BackendProviderKindError::Unknown {
                kind: other.to_owned(),
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAiCompatible",
            Self::OpenRouter => "OpenRouter",
            Self::ChatGptCodex => "ChatGptCodex",
            Self::XaiGrokOAuth => "XaiGrokOAuth",
            Self::ClaudeCliSubscription => "ClaudeCliSubscription",
            Self::AnthropicApiKey => "AnthropicApiKey",
        }
    }

    /// Must not be fleet-probed because credentials are node-scoped documents.
    pub fn is_node_scoped_oauth(self) -> bool {
        matches!(
            self,
            Self::ChatGptCodex | Self::XaiGrokOAuth | Self::ClaudeCliSubscription
        )
    }

    pub fn uses_messages_wire(self) -> bool {
        matches!(self, Self::ClaudeCliSubscription | Self::AnthropicApiKey)
    }
}

impl std::fmt::Display for BackendProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
