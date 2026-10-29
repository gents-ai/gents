use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use gents::{BackendProviderKind, OpenAiWireApi};

use super::{ACCOUNT_HELP, ACCOUNT_PROVIDERS, ConfigListArgs, ConfigShowArgs};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum OpenAiWireApiArg {
    Responses,
    #[value(name = "chat-completions")]
    ChatCompletions,
}

impl OpenAiWireApiArg {
    pub(crate) fn to_config(self) -> OpenAiWireApi {
        match self {
            Self::Responses => OpenAiWireApi::Responses,
            Self::ChatCompletions => OpenAiWireApi::ChatCompletions,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum BackendPresetArg {
    #[value(name = "generic-openai-compatible")]
    GenericOpenAiCompatible,
    #[value(name = "openai")]
    OpenAi,
    #[value(name = "openrouter")]
    OpenRouter,
    #[value(name = "chatgpt-codex")]
    ChatGptCodex,
    #[value(name = "xai-oauth")]
    XaiGrokOAuth,
    #[value(name = "xai")]
    XaiApiKey,
    #[value(
        name = "claude-cli-subscription",
        alias = "claude-subscription",
        alias = "claude-cli"
    )]
    ClaudeCliSubscription,
    #[value(name = "anthropic")]
    Anthropic,
    #[value(name = "ollama")]
    Ollama,
    #[value(name = "vllm")]
    Vllm,
    #[value(name = "llama-cpp")]
    LlamaCpp,
}

impl BackendPresetArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::GenericOpenAiCompatible => "generic-openai-compatible",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::ChatGptCodex => "chatgpt-codex",
            Self::XaiGrokOAuth => "xai-oauth",
            Self::XaiApiKey => "xai",
            Self::ClaudeCliSubscription => "claude-cli-subscription",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::LlamaCpp => "llama-cpp",
        }
    }

    pub(crate) fn provider_kind(self) -> BackendProviderKind {
        match self {
            Self::OpenRouter => BackendProviderKind::OpenRouter,
            Self::ChatGptCodex => BackendProviderKind::ChatGptCodex,
            Self::XaiGrokOAuth => BackendProviderKind::XaiGrokOAuth,
            Self::ClaudeCliSubscription => BackendProviderKind::ClaudeCliSubscription,
            Self::Anthropic => BackendProviderKind::AnthropicApiKey,
            Self::GenericOpenAiCompatible
            | Self::OpenAi
            | Self::XaiApiKey
            | Self::Ollama
            | Self::Vllm
            | Self::LlamaCpp => BackendProviderKind::OpenAiCompatible,
        }
    }

    pub(crate) fn default_endpoint(self) -> Option<&'static str> {
        match self {
            Self::GenericOpenAiCompatible => None,
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::ChatGptCodex => Some(gents::chatgpt_codex::default_backend_endpoint()),
            Self::XaiGrokOAuth => Some(gents::xai_grok_oauth::default_backend_endpoint()),
            Self::XaiApiKey => Some(gents::inference_setup::XAI_API_ENDPOINT),
            Self::ClaudeCliSubscription => {
                Some(gents::claude_subscription::default_backend_endpoint())
            }
            Self::Anthropic => Some(gents::claude_subscription::ANTHROPIC_API_ENDPOINT),
            Self::Ollama => Some(crate::DEFAULT_OLLAMA_ENDPOINT),
            Self::Vllm => Some("http://127.0.0.1:8000/v1"),
            Self::LlamaCpp => Some("http://127.0.0.1:8080/v1"),
        }
    }

    pub(crate) fn default_model_name(self) -> Option<&'static str> {
        match self {
            Self::Ollama => Some(crate::DEFAULT_OLLAMA_MODEL_NAME),
            Self::LlamaCpp => Some(crate::DEFAULT_INIT_MODEL_NAME),
            Self::ChatGptCodex => Some(crate::DEFAULT_CHATGPT_CODEX_MODEL_NAME),
            Self::XaiGrokOAuth => Some(crate::DEFAULT_XAI_GROK_OAUTH_MODEL_NAME),
            Self::ClaudeCliSubscription => Some(gents::claude_subscription::default_model_name()),
            Self::GenericOpenAiCompatible
            | Self::OpenAi
            | Self::OpenRouter
            | Self::Anthropic
            | Self::XaiApiKey
            | Self::Vllm => None,
        }
    }

    pub(crate) fn default_api_key_env_var(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::XaiApiKey => Some("XAI_API_KEY"),
            Self::GenericOpenAiCompatible
            | Self::ChatGptCodex
            | Self::XaiGrokOAuth
            | Self::ClaudeCliSubscription
            | Self::Ollama
            | Self::Vllm
            | Self::LlamaCpp => None,
        }
    }

    pub(crate) fn default_openai_wire_api(self) -> Option<OpenAiWireApi> {
        match self {
            Self::OpenAi | Self::XaiApiKey => Some(OpenAiWireApi::Responses),
            Self::GenericOpenAiCompatible
            | Self::OpenRouter
            | Self::ChatGptCodex
            | Self::XaiGrokOAuth
            | Self::ClaudeCliSubscription
            | Self::Anthropic
            | Self::Ollama
            | Self::Vllm
            | Self::LlamaCpp => None,
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum InferenceProfileCommand {
    #[command(name = "set")]
    Set(InferenceProfileSetArgs),
    #[command(
        name = "set-account",
        about = "Move a profile to another account of its provider, or list where it can move"
    )]
    SetAccount(InferenceProfileSetAccountArgs),
    #[command(name = "list", about = "List InferenceProfile documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show an InferenceProfile document")]
    Show(ConfigShowArgs),
    #[command(name = "rm", about = "Delete an InferenceProfile document")]
    Rm(ConfigShowArgs),
}

#[derive(clap::Args)]
pub(crate) struct InferenceProfileSetArgs {
    /// Canonical InferenceProfile JSON document, including its owning node DID.
    #[arg(long)]
    pub(crate) file: PathBuf,
    /// Create or move the profile on this account; fills the document's backend_id.
    #[arg(long, help = ACCOUNT_HELP)]
    pub(crate) account: Option<String>,
    /// Narrows --account; alone, picks the provider's earliest-connected enabled account.
    #[arg(long, value_parser = ACCOUNT_PROVIDERS)]
    pub(crate) provider: Option<String>,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct InferenceProfileSetAccountArgs {
    /// The profile_id to move.
    pub(crate) profile: String,
    #[arg(
        help = "The account: its label, credential_id or account_ref, or an API-key backend's \
                id or name; without it, list the accounts the profile can move to"
    )]
    pub(crate) account: Option<String>,
    #[arg(long, value_parser = ACCOUNT_PROVIDERS, help = "Narrows the account to this sign-in provider")]
    pub(crate) provider: Option<String>,
    /// Also move the other profiles of the profile's agents on the same account.
    #[arg(long, requires = "account")]
    pub(crate) with_compaction: bool,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct BackendSetArgs {
    /// Canonical InferenceBackend JSON document, including its owning node DID.
    #[arg(long)]
    pub(crate) file: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct BackendDiscoverModelsArgs {
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(
        long,
        help = "Discover this stored backend and publish its credential-free model catalog observation"
    )]
    pub(crate) backend_id: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Backend preset with provider/auth defaults for common local and hosted backends"
    )]
    pub(crate) backend_preset: Option<BackendPresetArg>,
    #[arg(
        long,
        help = "Backend provider kind. OpenAiCompatible covers OpenAI-style local and hosted endpoints"
    )]
    pub(crate) provider_kind: Option<String>,
    #[arg(
        long,
        help = "Inference backend base URL, usually including /v1. Falls back to the preset default when available"
    )]
    pub(crate) endpoint: Option<String>,
    #[arg(long, help = "Raw API key to use for this probe only")]
    pub(crate) api_key: Option<String>,
    #[arg(long, help = "Environment variable name holding the probe API key")]
    pub(crate) api_key_env_var: Option<String>,
    #[arg(
        long,
        help = "Node DID owning the OAuth credential (defaults to the local node). Only used for OAuth-credential backends (ChatGptCodex, XaiGrokOAuth, ClaudeCliSubscription), whose bearer is a DefraDB document rather than an api_key"
    )]
    pub(crate) node_did: Option<String>,
    #[arg(
        long,
        help = "Node home directory used to resolve the local node DID for OAuth-credential backend discovery (defaults to ~/.gents). Pass --node-did instead to target a specific node"
    )]
    pub(crate) home: Option<PathBuf>,
}
