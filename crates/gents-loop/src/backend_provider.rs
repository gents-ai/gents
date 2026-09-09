//! The backend provider vocabulary the loop's request-projection layer
//! (`provider_input`) switches on. `gents`'s own `backend_provider` module
//! carries the rest (model discovery over reqwest, OAuth guidance) and
//! re-exports this enum so its callers see one type.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
    /// Claude subscription over Messages HTTP, authenticated with an
    /// agent-scoped `OAuthCredential` (`claude-subscription`) written by
    /// `gents claude-login`.
    #[serde(rename = "ClaudeCliSubscription")]
    ClaudeCliSubscription,
}

impl BackendProviderKind {
    pub fn parse_optional(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => anyhow::bail!("backend provider kind is required"),
            Some("OpenAiCompatible") => Ok(Self::OpenAiCompatible),
            Some("OpenRouter") => Ok(Self::OpenRouter),
            Some("ChatGptCodex") => Ok(Self::ChatGptCodex),
            Some("XaiGrokOAuth") => Ok(Self::XaiGrokOAuth),
            Some("ClaudeCliSubscription") => Ok(Self::ClaudeCliSubscription),
            Some(other) => anyhow::bail!("unknown backend provider kind {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAiCompatible",
            Self::OpenRouter => "OpenRouter",
            Self::ChatGptCodex => "ChatGptCodex",
            Self::XaiGrokOAuth => "XaiGrokOAuth",
            Self::ClaudeCliSubscription => "ClaudeCliSubscription",
        }
    }

    /// Backends that authenticate with agent-scoped `OAuthCredential` documents
    /// rather than a fleet-global API key. These must not be fleet-probed.
    pub fn is_agent_scoped_oauth(self) -> bool {
        matches!(
            self,
            Self::ChatGptCodex | Self::XaiGrokOAuth | Self::ClaudeCliSubscription
        )
    }
}

impl std::fmt::Display for BackendProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
