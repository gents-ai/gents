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
    #[serde(rename = "ClaudeBedrock")]
    ClaudeBedrock,
    #[serde(rename = "ClaudeVertex")]
    ClaudeVertex,
    #[serde(rename = "ClaudeFoundry")]
    ClaudeFoundry,
    #[serde(rename = "ClaudeAnthropicAws")]
    ClaudeAnthropicAws,
    #[serde(rename = "ClaudeMantle")]
    ClaudeMantle,
    #[serde(rename = "ClaudeGoogleCloud")]
    ClaudeGoogleCloud,
    #[serde(rename = "ClaudeGateway")]
    ClaudeGateway,
}

impl BackendProviderKind {
    pub const ALL: [Self; 13] = [
        Self::OpenAiCompatible,
        Self::OpenRouter,
        Self::ChatGptCodex,
        Self::XaiGrokOAuth,
        Self::ClaudeCliSubscription,
        Self::AnthropicApiKey,
        Self::ClaudeBedrock,
        Self::ClaudeVertex,
        Self::ClaudeFoundry,
        Self::ClaudeAnthropicAws,
        Self::ClaudeMantle,
        Self::ClaudeGoogleCloud,
        Self::ClaudeGateway,
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
            Some("ClaudeBedrock") => Ok(Self::ClaudeBedrock),
            Some("ClaudeVertex") => Ok(Self::ClaudeVertex),
            Some("ClaudeFoundry") => Ok(Self::ClaudeFoundry),
            Some("ClaudeAnthropicAws") => Ok(Self::ClaudeAnthropicAws),
            Some("ClaudeMantle") => Ok(Self::ClaudeMantle),
            Some("ClaudeGoogleCloud") => Ok(Self::ClaudeGoogleCloud),
            Some("ClaudeGateway") => Ok(Self::ClaudeGateway),
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
            Self::ClaudeBedrock => "ClaudeBedrock",
            Self::ClaudeVertex => "ClaudeVertex",
            Self::ClaudeFoundry => "ClaudeFoundry",
            Self::ClaudeAnthropicAws => "ClaudeAnthropicAws",
            Self::ClaudeMantle => "ClaudeMantle",
            Self::ClaudeGoogleCloud => "ClaudeGoogleCloud",
            Self::ClaudeGateway => "ClaudeGateway",
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
        self.claude_provider().is_some()
    }

    /// Claude Code's provider for a Messages-wire kind (`Te()`); `None` for
    /// every other wire. The subscription and the API key are first party.
    pub fn claude_provider(self) -> Option<gents_claude::betas::Provider> {
        use gents_claude::betas::Provider;
        match self {
            Self::ClaudeCliSubscription | Self::AnthropicApiKey => Some(Provider::FirstParty),
            Self::ClaudeBedrock => Some(Provider::Bedrock),
            Self::ClaudeVertex => Some(Provider::Vertex),
            Self::ClaudeFoundry => Some(Provider::Foundry),
            Self::ClaudeAnthropicAws => Some(Provider::AnthropicAws),
            Self::ClaudeMantle => Some(Provider::Mantle),
            Self::ClaudeGoogleCloud => Some(Provider::AnthropicGoogleCloud),
            Self::ClaudeGateway => Some(Provider::Gateway),
            _ => None,
        }
    }

    /// A Claude platform served by a cloud, a gateway or a partner endpoint:
    /// the Messages wire with an `endpoint` base URL and `claude_platform` settings.
    pub fn is_claude_platform(self) -> bool {
        self.uses_messages_wire()
            && !matches!(self, Self::ClaudeCliSubscription | Self::AnthropicApiKey)
    }
}

impl std::fmt::Display for BackendProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
