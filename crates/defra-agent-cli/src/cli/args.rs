// Soft-cap justified: clap type definitions are a tightly-coupled unit.
// Splitting by subcommand would fragment the command tree declaration.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use defra_agent::BackendProviderKind;
use serde::{Deserialize, Serialize};

use crate::{
    BACKGROUND_AFTER_HELP, CHAT_AFTER_HELP, CLI_AFTER_HELP, CONFIG_AFTER_HELP,
    CONFIG_EXPORT_AFTER_HELP, CONFIG_IMPORT_AFTER_HELP, DEFAULT_INIT_ENDPOINT, DIAGNOSE_AFTER_HELP,
    FLEET_AFTER_HELP, INIT_AFTER_HELP, MCP_AFTER_HELP, P2P_AFTER_HELP, PROVISION_AFTER_HELP,
    REQUEST_AFTER_HELP, RESET_AFTER_HELP, RESPONSE_AFTER_HELP, SCHEMA_AFTER_HELP,
    SERVER_AFTER_HELP, SESSION_AFTER_HELP, SHOW_AFTER_HELP, STATUS_AFTER_HELP, SUBAGENT_AFTER_HELP,
    SUBAGENT_LIST_AFTER_HELP, TOOLS_AFTER_HELP, TRACE_AFTER_HELP,
};

use crate::default_backend_max_queue_depth;

#[derive(Parser)]
#[command(
    name = "defra-agent",
    about = "Local-first CLI for bootstrapping, running, and inspecting a defra-agent runtime",
    after_help = CLI_AFTER_HELP
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "Print build and release metadata")]
    Version,
    #[command(about = "Initialize a local agent home directory", after_help = INIT_AFTER_HELP)]
    Init(InitArgs),
    #[command(
        about = "Provision a local agent home from a portable manifest root",
        after_help = PROVISION_AFTER_HELP
    )]
    Provision(ProvisionArgs),
    #[command(about = "Clear persisted local runtime state", after_help = RESET_AFTER_HELP)]
    Reset(ResetArgs),
    #[command(
        name = "server",
        alias = "serve",
        about = "Run the local defra-agent runtime from an initialized home",
        after_help = SERVER_AFTER_HELP
    )]
    Server(ServeArgs),
    #[command(about = "Chat with the local agent in the terminal", after_help = CHAT_AFTER_HELP)]
    Chat(ChatArgs),
    #[command(about = "Probe an existing Codex ChatGPT OAuth session")]
    CodexAuthProbe(CodexAuthProbeArgs),
    #[command(name = "__native-fs-runner", hide = true)]
    NativeFsRunner(NativeFsRunnerArgs),
    #[command(about = "Inspect and control live P2P runtime connectivity", after_help = P2P_AFTER_HELP)]
    P2p {
        #[command(subcommand)]
        command: P2pCommand,
    },
    #[command(about = "Apply app-specific collection schemas", after_help = SCHEMA_AFTER_HELP)]
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    #[command(about = "Show stored runtime, request, or response state", after_help = SHOW_AFTER_HELP)]
    Show {
        #[command(subcommand)]
        command: ShowCommand,
    },
    #[command(
        about = "Export persisted tool-call traces for measurement",
        after_help = TRACE_AFTER_HELP
    )]
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
    },
    #[command(about = "Show the current local runtime status", after_help = STATUS_AFTER_HELP)]
    Status(StatusArgs),
    #[command(about = "Run a read-only structured query against a DefraDB collection")]
    Query(QueryArgs),
    #[command(
        about = "Inspect backgrounded tool calls",
        after_help = BACKGROUND_AFTER_HELP
    )]
    Background {
        #[command(subcommand)]
        command: BackgroundCommand,
    },
    #[command(about = "Probe registered MCP service health", after_help = MCP_AFTER_HELP)]
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    #[command(about = "Inspect fleet admission slot accounting", after_help = FLEET_AFTER_HELP)]
    Fleet {
        #[command(subcommand)]
        command: FleetCommand,
    },
    #[command(about = "Run local configuration and runtime diagnostics", after_help = DIAGNOSE_AFTER_HELP)]
    Diagnose(DiagnoseArgs),
    #[command(about = "Explain resolved behavior tool surfaces", after_help = TOOLS_AFTER_HELP)]
    Tools {
        #[command(subcommand)]
        command: ToolsCommand,
    },
    #[command(about = "Inspect and write runtime configuration documents", after_help = CONFIG_AFTER_HELP)]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(about = "Low-level request submission and inspection", after_help = REQUEST_AFTER_HELP)]
    Request {
        #[command(subcommand)]
        command: RequestCommand,
    },
    #[command(about = "Low-level response inspection", after_help = RESPONSE_AFTER_HELP)]
    Response {
        #[command(subcommand)]
        command: ResponseCommand,
    },
    #[command(about = "Manage and fork agent sessions", after_help = SESSION_AFTER_HELP)]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    #[command(
        about = "Inspect and control background subagents",
        after_help = SUBAGENT_AFTER_HELP
    )]
    Subagent {
        #[command(subcommand)]
        command: SubagentCommand,
    },
}

#[derive(clap::Args)]
pub(crate) struct NativeFsRunnerArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: Option<PathBuf>,
    #[arg(long, value_name = "BASE")]
    pub(crate) base: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub(crate) self_test: bool,
}

#[derive(clap::Args)]
pub(crate) struct CodexAuthProbeArgs {
    #[arg(
        long,
        env = "DEFRA_CODEX_HOME",
        value_name = "CODEX_HOME",
        help = "Codex home directory to read. Defaults to ~/.codex"
    )]
    pub(crate) codex_home: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = 20,
        help = "Maximum number of model slugs to print"
    )]
    pub(crate) max_models: usize,
}

#[derive(clap::Args)]
pub(crate) struct ProvisionArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        value_name = "ROOT",
        help = "Portable manifest root to bind to this home and apply"
    )]
    pub(crate) root: PathBuf,
    #[arg(
        long,
        help = "Local display name and default key filename when the home has not been initialized. Defaults to the manifest root directory name."
    )]
    pub(crate) agent_name: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Create a local file-key identity when the home is uninitialized. Production hosts should bootstrap identity first."
    )]
    pub(crate) bootstrap_file_identity: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Create/load a macOS Secure Enclave identity when the home is uninitialized."
    )]
    pub(crate) bootstrap_macos_secure_enclave: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Create/load a macOS login-keychain software identity when the home is uninitialized."
    )]
    pub(crate) bootstrap_macos_keychain: bool,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for the macOS keychain identity."
    )]
    pub(crate) keychain_label: Option<String>,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for the macOS Secure Enclave identity."
    )]
    pub(crate) secure_enclave_label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum IdentityBackendArg {
    File,
    MacosKeychain,
    MacosSecureEnclave,
}

#[derive(clap::Args)]
pub(crate) struct InitArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete the existing home directory before re-initializing it"
    )]
    pub(crate) dangerously_overwrite: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Clear persisted local runtime state after initialization"
    )]
    pub(crate) reset: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Create/load identity and write init.json without seeding runtime config documents"
    )]
    pub(crate) identity_only: bool,
    #[arg(long, default_value = crate::DEFAULT_AGENT_NAME, help = "Local display name and default key filename. The agent DID is derived from the identity key.")]
    pub(crate) agent_name: String,
    #[arg(long)]
    pub(crate) key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        default_value_t = IdentityBackendArg::File,
        help = "Local identity backend for init metadata."
    )]
    pub(crate) identity_backend: IdentityBackendArg,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for --identity-backend macos-keychain."
    )]
    pub(crate) keychain_label: Option<String>,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for --identity-backend macos-secure-enclave."
    )]
    pub(crate) secure_enclave_label: Option<String>,
    #[arg(
        long = "inference-url",
        alias = "inference-endpoint",
        value_name = "INFERENCE_URL",
        help = "Inference backend base URL, usually including /v1. Falls back to INFERENCE_ENDPOINT, then local Ollama."
    )]
    pub(crate) inference_endpoint: Option<String>,
    #[arg(value_name = "INFERENCE_URL", hide = true)]
    pub(crate) inference_endpoint_legacy: Option<String>,
    #[arg(
        long,
        help = "Optional backend document id. Defaults to <agent-name>-backend"
    )]
    pub(crate) backend_id: Option<String>,
    #[arg(
        long,
        help = "Optional backend display name. Defaults to the backend id"
    )]
    pub(crate) backend_name: Option<String>,
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
    #[arg(long, help = "Raw API key stored directly in the backend document")]
    pub(crate) api_key: Option<String>,
    #[arg(long, help = "Environment variable name holding the backend API key")]
    pub(crate) api_key_env_var: Option<String>,
    #[arg(
        long,
        default_value = crate::DEFAULT_INIT_MODEL_NAME,
        help = "Model id to bind to the default behavior"
    )]
    pub(crate) model_name: String,
    #[arg(long, default_value_t = 2)]
    pub(crate) max_concurrent: i64,
    #[arg(long, default_value_t = default_backend_max_queue_depth())]
    pub(crate) max_queue_depth: i64,
    #[arg(
        long,
        default_value_t = false,
        help = "Bootstrap write-capable tools instead of the safe read-only default"
    )]
    pub(crate) write_tools: bool,
    #[arg(
        long,
        value_enum,
        help = "Bootstrap a named tool package. Defaults to readonly; --write-tools is a compatibility alias for --tool-package write"
    )]
    pub(crate) tool_package: Option<ToolPackageArg>,
    #[arg(
        long,
        help = "Root directory for local file/bash tools. Defaults to the current working directory"
    )]
    pub(crate) tool_root: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Enable the feature-gated per-agent memory tool in the default ToolSelection"
    )]
    pub(crate) enable_memory: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Disable the default read-only defra_query tool in the default ToolSelection"
    )]
    pub(crate) disable_defra_query: bool,
    #[arg(
        long = "defra-query-collection",
        help = "Restrict init's default defra_query tool to these collections (repeatable); omit for all collections"
    )]
    pub(crate) defra_query_collections: Vec<String>,
}

impl InitArgs {
    pub(crate) fn resolved_inference_endpoint(&self) -> Option<&str> {
        self.inference_endpoint
            .as_deref()
            .or(self.inference_endpoint_legacy.as_deref())
    }
}

#[derive(clap::Args)]
pub(crate) struct ResetArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct ServeArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1")]
    pub(crate) http_addr: IpAddr,
    #[arg(long, default_value_t = crate::DEFAULT_HTTP_PORT)]
    pub(crate) http_port: u16,
    #[arg(long)]
    pub(crate) agent_name: Option<String>,
    #[arg(long)]
    pub(crate) key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "Operator safety cap that clamps document tool selection at runtime"
    )]
    pub(crate) tool_ceiling: Option<ToolCeilingArg>,
    #[arg(long = "cli-tool")]
    pub(crate) cli_tools: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Expose the read-only defra_query MCP tool at /mcp. Off by default: this is an unauthenticated read surface (same listener exposure as the GraphQL endpoint)"
    )]
    pub(crate) enable_mcp: bool,
    #[arg(
        long = "mcp-query-collection",
        help = "When --enable-mcp is set, restrict the /mcp defra_query tool to these collections (repeatable); omit for all"
    )]
    pub(crate) mcp_query_collections: Vec<String>,
    #[arg(
        long,
        help = "Root directory for readonly/readwrite tool ceilings. Readonly defaults to the current working directory when unset"
    )]
    pub(crate) tool_root: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Also run the experimental Codex TUI compatibility endpoint"
    )]
    pub(crate) codex_shim: bool,
    #[arg(
        long,
        default_value = "127.0.0.1",
        help = "Address for the Codex shim to listen on. Use a specific trusted private/Tailscale IP for remote Codex clients; default is loopback only"
    )]
    pub(crate) codex_shim_bind_addr: IpAddr,
    #[arg(long, default_value_t = crate::DEFAULT_CODEX_SHIM_PORT)]
    pub(crate) codex_shim_port: u16,
    #[arg(long, help = "Optional DEFRA behavior override for Codex turns")]
    pub(crate) codex_shim_behavior_id: Option<String>,
    #[arg(long, default_value_t = crate::DEFAULT_CODEX_SHIM_TIMEOUT_SECS)]
    pub(crate) codex_shim_timeout_secs: u64,
    #[arg(long, default_value_t = 250)]
    pub(crate) codex_shim_poll_ms: u64,
    #[arg(long)]
    pub(crate) p2p_bind_addr: Option<IpAddr>,
    #[arg(long)]
    pub(crate) p2p_port: Option<u16>,
    #[arg(long)]
    pub(crate) p2p_secret_key_path: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = P2pRelayModeArg::Disabled)]
    pub(crate) p2p_relay_mode: P2pRelayModeArg,
    #[arg(long, value_enum, default_value_t = P2pDiscoveryArg::Disabled)]
    pub(crate) p2p_discovery: P2pDiscoveryArg,
}

#[derive(clap::Args)]
pub(crate) struct ChatArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
    #[arg(long)]
    pub(crate) agent_name: Option<String>,
    #[arg(
        long,
        help = "Continue an existing session instead of starting a fresh one"
    )]
    pub(crate) session_id: Option<String>,
    #[arg(long, help = "Override the behavior for this one-off turn or session")]
    pub(crate) behavior_id: Option<String>,
    #[arg(long = "message-file", help = "Read the user message from a file")]
    pub(crate) message_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = ChatOutputFormat::Text)]
    pub(crate) output_format: ChatOutputFormat,
    #[arg(long = "output-file", help = "Write the final response to a file")]
    pub(crate) output_file: Option<PathBuf>,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
    #[arg(value_name = "MESSAGE")]
    pub(crate) message: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum ShowCommand {
    #[command(about = "Show a stored AgentRequest document")]
    Request(RequestShowArgs),
    #[command(about = "Show the latest AgentResponse for a request")]
    Response(ResponseShowArgs),
    #[command(about = "Show the persisted AgentRuntime document")]
    Runtime(RuntimeShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum BackgroundCommand {
    #[command(
        name = "list",
        about = "List backgrounded AgentToolCall rows",
        after_help = BACKGROUND_AFTER_HELP
    )]
    List(BackgroundListArgs),
}

#[derive(Subcommand)]
pub(crate) enum McpCommand {
    #[command(
        name = "probe",
        about = "Run a one-shot health probe for registered MCP services",
        after_help = MCP_AFTER_HELP
    )]
    Probe(McpProbeArgs),
}

#[derive(clap::Args)]
pub(crate) struct McpProbeArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "GraphQL endpoint to read registry rows instead of local home state"
    )]
    pub(crate) graphql: Option<String>,
    #[arg(long, action = ArgAction::SetTrue, help = "Probe every online MCP service")]
    pub(crate) all: bool,
    #[arg(long, default_value = "5s", value_name = "DURATION")]
    pub(crate) timeout: String,
    #[arg(long, value_enum, default_value_t = McpProbeOutput::Text)]
    pub(crate) output: McpProbeOutput,
    #[arg(value_name = "SERVICE")]
    pub(crate) service: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum McpProbeOutput {
    Text,
    Json,
}

#[derive(Subcommand)]
pub(crate) enum FleetCommand {
    #[command(
        name = "slots",
        about = "Show derived fleet slot usage from the live runtime HTTP API",
        after_help = FLEET_AFTER_HELP
    )]
    Slots(FleetSlotsArgs),
}

#[derive(clap::Args)]
pub(crate) struct BackgroundListArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(
        long = "request",
        value_name = "ID",
        help = "Only show backgrounded tools for this parent request"
    )]
    pub(crate) request_id: Option<String>,
    #[arg(
        long,
        value_name = "STATE",
        help = "Only show tool calls whose displayed state matches this value"
    )]
    pub(crate) state: Option<String>,
    #[arg(
        long,
        value_name = "DURATION",
        help = "Only show calls older than this duration, e.g. 30s, 5m, 2h"
    )]
    pub(crate) age_gt: Option<String>,
    #[arg(long, value_enum, default_value_t = BackgroundOutputFormat::Table)]
    pub(crate) output: BackgroundOutputFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum BackgroundOutputFormat {
    Table,
    Json,
}

#[derive(clap::Args)]
pub(crate) struct FleetSlotsArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the live runtime")]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct StatusArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct QueryArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Collection (GraphQL type) to read, e.g. AgentRequest")]
    pub(crate) collection: String,
    #[arg(
        long = "field",
        help = "Field to return (repeatable); at least one is required"
    )]
    pub(crate) fields: Vec<String>,
    #[arg(
        long,
        help = r#"DefraDB filter as JSON, e.g. '{"status":{"_eq":"completed"}}'"#
    )]
    pub(crate) filter: Option<String>,
    #[arg(long, help = "Maximum rows to return (default 50, capped at 1000)")]
    pub(crate) limit: Option<u32>,
    #[arg(
        long = "allow-collection",
        help = "Restrict the query to these collections (repeatable); omit for all"
    )]
    pub(crate) allow_collections: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ChatOutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum P2pTransportArg {
    None,
    Iroh,
}

impl P2pTransportArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Iroh => "iroh",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum P2pRelayModeArg {
    Default,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum P2pDiscoveryArg {
    #[value(name = "n0")]
    N0,
    Disabled,
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
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::LlamaCpp => "llama-cpp",
        }
    }

    pub(crate) fn provider_kind(self) -> BackendProviderKind {
        match self {
            Self::OpenRouter => BackendProviderKind::OpenRouter,
            Self::ChatGptCodex => BackendProviderKind::ChatGptCodex,
            Self::GenericOpenAiCompatible
            | Self::OpenAi
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
            Self::ChatGptCodex => Some(defra_agent::chatgpt_codex::default_backend_endpoint()),
            Self::Ollama => Some(DEFAULT_INIT_ENDPOINT),
            Self::Vllm => Some("http://127.0.0.1:8000/v1"),
            Self::LlamaCpp => Some("http://127.0.0.1:8080/v1"),
        }
    }

    pub(crate) fn default_api_key_env_var(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::GenericOpenAiCompatible
            | Self::ChatGptCodex
            | Self::Ollama
            | Self::Vllm
            | Self::LlamaCpp => None,
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct DiagnoseArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
    #[arg(long = "bind-agent-did", value_enum)]
    pub(crate) bind_agent_did: Option<ManifestAgentDidBindingArg>,
}

#[derive(clap::Args)]
pub(crate) struct RuntimeShowArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum TraceCommand {
    #[command(name = "export", about = "Export Amy-style tool-call JSONL")]
    Export(TraceExportArgs),
}

#[derive(clap::Args)]
pub(crate) struct TraceExportArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Restrict export to one session_id")]
    pub(crate) session_id: Option<String>,
    #[arg(long, help = "Restrict export to one inferred request_id")]
    pub(crate) request_id: Option<String>,
    #[arg(long, help = "Run id to stamp on exported JSONL records")]
    pub(crate) run_id: Option<String>,
    #[arg(long, help = "Case id to stamp on exported JSONL records")]
    pub(crate) case_id: Option<String>,
    #[arg(
        long,
        default_value_t = 500,
        help = "Maximum recent AgentToolCall rows to export"
    )]
    pub(crate) limit: usize,
    #[arg(long = "output-file", help = "Write JSONL to a file instead of stdout")]
    pub(crate) output_file: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    #[command(about = "Validate desired-state manifests under a repository root")]
    Validate(ConfigValidateArgs),
    #[command(about = "Diff desired-state manifests against live configuration")]
    Diff(ConfigDiffArgs),
    #[command(about = "Apply desired-state manifests to live configuration")]
    Apply(ConfigApplyArgs),
    #[command(about = "Write an InferenceBackend document")]
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
    },
    #[command(about = "Write an AgentBehavior document")]
    Behavior {
        #[command(subcommand)]
        command: BehaviorCommand,
    },
    #[command(about = "Write a ToolSelection document")]
    Tools {
        #[command(subcommand)]
        command: ToolSelectionCommand,
    },
    #[command(about = "Write an InferenceProfile document")]
    Profile {
        #[command(subcommand)]
        command: InferenceProfileCommand,
    },
    #[command(about = "Inspect and fire configured Task documents")]
    Task {
        #[command(subcommand)]
        command: ConfigTaskCommand,
    },
    #[command(about = "Create, list, show, delete, enable, or disable Skill documents")]
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    #[command(about = "Export desired configuration documents", after_help = CONFIG_EXPORT_AFTER_HELP)]
    Export(ConfigExportArgs),
    #[command(about = "Import desired configuration documents", after_help = CONFIG_IMPORT_AFTER_HELP)]
    Import(ConfigImportArgs),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
pub(crate) enum ToolCeilingArg {
    MetaOnly,
    Readonly,
    Readwrite,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
pub(crate) enum ToolPackageArg {
    Minimal,
    Introspection,
    Readonly,
    Write,
}

#[derive(Subcommand)]
pub(crate) enum BackendCommand {
    #[command(name = "set")]
    Set(BackendUpsertArgs),
    #[command(name = "discover-models")]
    DiscoverModels(BackendDiscoverModelsArgs),
}

#[derive(Subcommand)]
pub(crate) enum BehaviorCommand {
    #[command(name = "set")]
    Set(BehaviorUpsertArgs),
}

#[derive(Subcommand)]
pub(crate) enum ToolSelectionCommand {
    #[command(name = "set")]
    Set(ToolSelectionUpsertArgs),
}

#[derive(Subcommand)]
pub(crate) enum SkillCommand {
    #[command(name = "add", about = "Create or update a Skill document")]
    Add(SkillAddArgs),
    #[command(
        name = "import",
        about = "Import a directory tree of Codex-format SKILL.md files as Skill documents"
    )]
    Import(SkillImportArgs),
    #[command(
        name = "export",
        about = "Export an agent's Skill documents as a SKILL.md directory tree"
    )]
    Export(SkillExportArgs),
    #[command(name = "list", about = "List Skill documents for an agent")]
    List(SkillListArgs),
    #[command(name = "show", about = "Show a single Skill document")]
    Show(SkillShowArgs),
    #[command(name = "rm", about = "Delete a Skill document")]
    Rm(SkillRefArgs),
    #[command(name = "enable", about = "Enable a Skill document")]
    Enable(SkillRefArgs),
    #[command(name = "disable", about = "Disable a Skill document")]
    Disable(SkillRefArgs),
}

#[derive(Subcommand)]
pub(crate) enum ToolsCommand {
    #[command(
        name = "explain",
        about = "Explain final model-callable tools per behavior"
    )]
    Explain(ToolExplainArgs),
}

#[derive(clap::Args)]
pub(crate) struct ToolExplainArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
    #[arg(long, help = "Only explain one behavior_id")]
    pub(crate) behavior_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct SkillAddArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) agent_did: String,
    #[arg(long)]
    pub(crate) skill_id: String,
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Activation scope: "principal" (inherited by all the agent's behaviors)
    /// or "behavior" (only where a behavior opts in via skill_refs).
    #[arg(long, default_value = "behavior")]
    pub(crate) scope: String,
    #[arg(long)]
    pub(crate) description: Option<String>,
    /// Inline skill instructions (the body composed into the prompt).
    #[arg(long)]
    pub(crate) instructions: Option<String>,
    /// Read instructions from a file (takes precedence over --instructions).
    #[arg(long)]
    pub(crate) instructions_file: Option<PathBuf>,
    /// Declared tool dependency (repeatable). Intersected with the behavior
    /// tool ceiling at activation; never grants a tool.
    #[arg(long = "tool-ref")]
    pub(crate) tool_refs: Vec<String>,
    #[arg(long)]
    pub(crate) display_name: Option<String>,
    #[arg(long, default_value_t = true)]
    pub(crate) enabled: bool,
}

#[derive(clap::Args)]
pub(crate) struct SkillImportArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) agent_did: String,
    /// Directory tree to scan for `SKILL.md` files (Codex skill layout:
    /// `<dir>/<skill-name>/SKILL.md` + optional `agents/openai.yaml`).
    #[arg(value_name = "DIR")]
    pub(crate) dir: PathBuf,
    /// Scope applied to every imported skill: "principal" or "behavior".
    #[arg(long, default_value = "behavior")]
    pub(crate) scope: String,
    /// Import skills as disabled.
    #[arg(long)]
    pub(crate) disabled: bool,
    /// Parse and report what would be imported without writing.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(clap::Args)]
pub(crate) struct SkillExportArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) agent_did: String,
    /// Output directory. Each skill is written to `<dir>/<skill_id>/SKILL.md`
    /// (plus `agents/openai.yaml` when it has tool_refs or a display name).
    #[arg(value_name = "DIR")]
    pub(crate) dir: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SkillListArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) agent_did: String,
}

#[derive(clap::Args)]
pub(crate) struct SkillShowArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) skill_id: String,
}

/// Shared args for skill commands that target a single skill by id.
#[derive(clap::Args)]
pub(crate) struct SkillRefArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) skill_id: String,
}

#[derive(clap::Args)]
pub(crate) struct BehaviorUpsertArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) agent_did: String,
    #[arg(long)]
    pub(crate) behavior_id: Option<String>,
    #[arg(long)]
    pub(crate) display_name: Option<String>,
    #[arg(long)]
    pub(crate) system_prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) backend_id: Option<String>,
    #[arg(long)]
    pub(crate) model_name: Option<String>,
    #[arg(long)]
    pub(crate) tool_selection_id: Option<String>,
    #[arg(long)]
    pub(crate) inference_profile_id: Option<String>,
    #[arg(long)]
    pub(crate) compaction_strategy: Option<String>,
    #[arg(long)]
    pub(crate) compaction_threshold: Option<f64>,
    #[arg(long, default_value_t = true)]
    pub(crate) enabled: bool,
}

#[derive(clap::Args)]
pub(crate) struct ToolSelectionUpsertArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) agent_did: String,
    #[arg(long)]
    pub(crate) selection_id: String,
    #[arg(long)]
    pub(crate) display_name: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_display_name: bool,
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL",
        help = "Enable or disable file tools. Omit to preserve existing setting"
    )]
    pub(crate) enable_file_tools: Option<bool>,
    #[arg(long)]
    pub(crate) file_tools_mode: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_file_tools_mode: bool,
    #[arg(
        long,
        help = "Optional per-behavior file-tool root; relative paths resolve from the daemon cwd and must stay within any node-level tool root"
    )]
    pub(crate) file_tool_root: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_file_tool_root: bool,
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL",
        help = "Enable or disable bash tools. Omit to preserve existing setting"
    )]
    pub(crate) enable_bash: Option<bool>,
    #[arg(long)]
    pub(crate) bash_mode: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_bash_mode: bool,
    #[arg(
        long,
        help = "Command policy for bash: read_only, workspace_write, managed_write, or unrestricted"
    )]
    pub(crate) command_execution_policy: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_command_execution_policy: bool,
    #[arg(
        long,
        help = "Network policy hint for bash commands: inherit, disabled, or enabled"
    )]
    pub(crate) command_network_mode: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_command_network_mode: bool,
    #[arg(long = "command-allowed-argv-prefix")]
    pub(crate) command_allowed_argv_prefixes: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_command_allowed_argv_prefixes: bool,
    #[arg(long = "command-forbidden-argv-prefix")]
    pub(crate) command_forbidden_argv_prefixes: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_command_forbidden_argv_prefixes: bool,
    #[arg(long = "cli-tool-name")]
    pub(crate) cli_tool_names: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_cli_tool_names: bool,
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL",
        help = "Enable or disable meta MCP tools. Omit to preserve existing setting"
    )]
    pub(crate) enable_meta_tools: Option<bool>,
    #[arg(long = "allowed-mcp-service-id")]
    pub(crate) allowed_mcp_service_ids: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_allowed_mcp_service_ids: bool,
    #[arg(
        long = "backgroundable-tool-name",
        help = "Host tool that may be spawned as a background process via spawn_process, e.g. bash_unrestricted"
    )]
    pub(crate) backgroundable_tool_names: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_backgroundable_tool_names: bool,
    #[arg(
        long,
        help = "Enable or disable the feature-gated memory tool: --enable-memory true|false. Omit to leave the existing document setting unchanged (default is disabled)"
    )]
    pub(crate) enable_memory: Option<bool>,
    #[arg(
        long,
        help = "Enable or disable the sessions history convenience tool: --enable-session-history-tool true|false. Omit to leave the existing document setting unchanged (default is disabled)"
    )]
    pub(crate) enable_session_history_tool: Option<bool>,
    #[arg(
        long,
        help = "Enable or disable the read-only defra_query tool: --enable-defra-query true|false. Omit to leave the existing document setting unchanged (default is enabled)"
    )]
    pub(crate) enable_defra_query: Option<bool>,
    #[arg(
        long = "defra-query-collection",
        help = "Restrict defra_query to these collections (repeatable); omit for all collections"
    )]
    pub(crate) defra_query_collections: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_defra_query_collections: bool,
    #[arg(
        long = "subagent-target",
        help = "SubagentTarget JSON entry allowed for spawn_subagent (repeatable); omit to preserve existing targets"
    )]
    pub(crate) subagent_targets: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Clear existing subagent_targets when no --subagent-target values are provided"
    )]
    pub(crate) clear_subagent_targets: bool,
    #[arg(
        long,
        help = "Enable or disable spawn_subagent: --subagent-spawn-enabled true|false. Omit to preserve existing setting"
    )]
    pub(crate) subagent_spawn_enabled: Option<bool>,
    #[arg(
        long,
        help = "Enable or disable subagent steering tools: --subagent-steering-enabled true|false. Omit to preserve existing setting"
    )]
    pub(crate) subagent_steering_enabled: Option<bool>,
    #[arg(
        long,
        help = "Enable or disable background subagent steering: --subagent-background-enabled true|false. Omit to preserve existing setting"
    )]
    pub(crate) subagent_background_enabled: Option<bool>,
    #[arg(
        long,
        help = "Enable or disable remote-DID subagent targets: --subagent-allow-cross-deployment true|false. Omit to preserve existing setting"
    )]
    pub(crate) subagent_allow_cross_deployment: Option<bool>,
    #[arg(
        long,
        help = "Cross-deployment spawn timeout in seconds. Omit to preserve existing setting"
    )]
    pub(crate) cross_deployment_spawn_timeout_seconds: Option<i64>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_cross_deployment_spawn_timeout_seconds: bool,
}

#[derive(Subcommand)]
pub(crate) enum InferenceProfileCommand {
    #[command(name = "set")]
    Set(InferenceProfileUpsertArgs),
}

#[derive(Subcommand)]
pub(crate) enum ConfigTaskCommand {
    #[command(name = "run", about = "Run a configured Task once, now")]
    Run(ConfigTaskRunArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct ConfigTaskRunArgs {
    /// The task_id of the task to run.
    #[arg(long)]
    pub(crate) task_id: String,

    /// JSON object of arguments bound as the `args.*` template scope.
    /// Example: `--args '{"name": "Amy"}'`.
    #[arg(long, default_value = "{}")]
    pub(crate) args: String,

    /// GraphQL endpoint of the running agent's DefraDB. Defaults to local.
    #[arg(long)]
    pub(crate) graphql: Option<String>,

    /// Path to the agent home. Used to resolve GraphQL endpoint when
    /// `--graphql` is not set.
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct InferenceProfileUpsertArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) profile_id: String,
    #[arg(long)]
    pub(crate) display_name: Option<String>,
    #[arg(long)]
    pub(crate) context_window: Option<i64>,
    #[arg(long)]
    pub(crate) max_output_tokens: Option<i64>,
    #[arg(long)]
    pub(crate) max_turns: Option<i64>,
    #[arg(long)]
    pub(crate) temperature: Option<f64>,
    #[arg(long)]
    pub(crate) stream_batch_ms: Option<i64>,
    #[arg(long)]
    pub(crate) deadline_duration_secs: Option<i64>,
}

#[derive(clap::Args)]
pub(crate) struct BackendUpsertArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) backend_id: String,
    #[arg(long)]
    pub(crate) name: String,
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
    #[arg(long, help = "Raw API key stored directly in the backend document")]
    pub(crate) api_key: Option<String>,
    #[arg(
        long,
        help = "Environment variable name holding this backend's API key"
    )]
    pub(crate) api_key_env_var: Option<String>,
    #[arg(long)]
    pub(crate) max_concurrent: i64,
    #[arg(long, default_value_t = default_backend_max_queue_depth())]
    pub(crate) max_queue_depth: i64,
    #[arg(long, default_value_t = true)]
    pub(crate) enabled: bool,
    #[arg(long, default_value = "healthy")]
    pub(crate) probe_status: String,
}

#[derive(clap::Args)]
pub(crate) struct BackendDiscoverModelsArgs {
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
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
}

#[derive(clap::Args)]
pub(crate) struct ConfigExportArgs {
    #[arg(
        long,
        value_name = "ROOT",
        help = "Directory to write the manifest root into (author format for `config validate`, `diff`, and `apply`)"
    )]
    pub(crate) root: PathBuf,
    #[arg(
        long,
        default_value_t = false,
        help = "Overwrite the root dir if it is non-empty"
    )]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
    #[arg(long = "bind-agent-did", value_enum)]
    pub(crate) bind_agent_did: Option<ManifestAgentDidBindingArg>,
}

#[derive(clap::Args)]
pub(crate) struct ConfigImportArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(
        long = "override",
        default_value_t = false,
        help = "Upsert documents instead of failing when they already exist"
    )]
    pub(crate) override_existing: bool,
    #[arg(
        value_name = "PATH",
        help = "Legacy JSON bundle file to import (separate from `config export --root` manifest-dir output; use `config apply --root <dir>` to apply manifest roots). Reads stdin when omitted"
    )]
    pub(crate) path: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct ConfigValidateArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "bind-agent-did", value_enum)]
    pub(crate) bind_agent_did: Option<ManifestAgentDidBindingArg>,
    #[arg(long, default_value_t = false)]
    pub(crate) force_rebind_concrete_did: bool,
}

#[derive(clap::Args)]
pub(crate) struct ConfigDiffArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "bind-agent-did", value_enum)]
    pub(crate) bind_agent_did: Option<ManifestAgentDidBindingArg>,
    #[arg(long, default_value_t = false)]
    pub(crate) force_rebind_concrete_did: bool,
}

#[derive(clap::Args)]
pub(crate) struct ConfigApplyArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "bind-agent-did", value_enum)]
    pub(crate) bind_agent_did: Option<ManifestAgentDidBindingArg>,
    #[arg(long, default_value_t = false)]
    pub(crate) force_rebind_concrete_did: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete live-only desired-state documents absent from the manifest, routed through the proven ApplyReconcile delete-safety model"
    )]
    pub(crate) prune: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ManifestAgentDidBindingArg {
    Home,
    Live,
}

#[derive(Subcommand)]
pub(crate) enum SchemaCommand {
    #[command(about = "Apply SDL and JSON Patch schema files")]
    Apply(SchemaApplyArgs),
}

#[derive(clap::Args)]
pub(crate) struct SchemaApplyArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "GraphQL endpoint to apply to instead of local home state"
    )]
    pub(crate) graphql: Option<String>,
    #[arg(
        value_name = "PATH",
        help = "Schema file or directory. Directories apply *.graphql/*.gql files, then *.patch.json/*.json-patch files"
    )]
    pub(crate) path: PathBuf,
    #[arg(
        long = "patch",
        value_name = "PATCH",
        help = "Extra JSON Patch file to apply after SDL files. May be repeated"
    )]
    pub(crate) patches: Vec<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum P2pCommand {
    #[command(about = "Show live P2P connectivity for the running runtime")]
    Status(P2pAccessArgs),
    #[command(about = "List connected peers for the running runtime")]
    Peers(P2pAccessArgs),
    #[command(about = "Connect the running runtime to another peer")]
    Connect(P2pConnectArgs),
    #[command(about = "Manage collection subscriptions for the running runtime")]
    Collections {
        #[command(subcommand)]
        command: P2pCollectionsCommand,
    },
    #[command(about = "Manage push replicators for the running runtime")]
    Replicators {
        #[command(subcommand)]
        command: P2pReplicatorsCommand,
    },
    #[command(about = "Manage document subscriptions and document sync")]
    Documents {
        #[command(subcommand)]
        command: P2pDocumentsCommand,
    },
    #[command(about = "Run P2P HTTP endpoint diagnostics")]
    Diagnose(P2pAccessArgs),
    #[command(
        about = "Set up delegation replication between this server and a peer (connect + collections + replicator)",
        after_help = "\
This command runs against the LOCAL server (--graphql / --home) and sets up \
one direction of replication toward --peer:\n\
  1. connect  — dials the peer\n\
  2. collections add  — subscribes the profile's collections\n\
  3. replicators add  — installs a push replicator to the peer\n\
\n\
Replication is directional. For bidirectional delegation (the child writes \
AgentRequests/AgentToolCalls that the parent reads, and the parent writes \
AgentResponses/AgentMessages that the child reads) you must run `p2p pair` on \
BOTH servers, each with --peer set to the other's listen address.\n\
\n\
NOTE: replication alone does NOT enable cross-deployment delegation. \
Cross-deployment delegation is off by default and DEFERRED — to opt in, \
set `subagent_allow_cross_deployment: true` on the relevant behaviors' tool \
selections on BOTH the orchestrator and the target server (trusted-fleet only)."
    )]
    Pair(P2pPairArgs),
}

#[derive(clap::Args)]
pub(crate) struct P2pAccessArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pConnectArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) peer: String,
}

#[derive(Subcommand)]
pub(crate) enum P2pCollectionsCommand {
    #[command(about = "List subscribed P2P collections")]
    List(P2pAccessArgs),
    #[command(about = "Subscribe collections or collection profiles for P2P replication")]
    Add(P2pCollectionsMutateArgs),
    #[command(about = "Remove subscribed P2P collections")]
    Remove(P2pCollectionsMutateArgs),
    #[command(about = "Fetch a branchable collection DAG from connected peers")]
    SyncBranchable(P2pSyncBranchableArgs),
    #[command(about = "Fetch collection-version DAG blocks from connected peers")]
    SyncVersions(P2pSyncVersionsArgs),
}

#[derive(Subcommand)]
pub(crate) enum P2pReplicatorsCommand {
    #[command(about = "List configured P2P replicators")]
    List(P2pAccessArgs),
    #[command(about = "Configure a peer replicator for collections or profiles")]
    Add(P2pReplicatorAddArgs),
    #[command(about = "Remove a peer replicator for collections or profiles")]
    Remove(P2pReplicatorRemoveArgs),
}

#[derive(Subcommand)]
pub(crate) enum P2pDocumentsCommand {
    #[command(about = "List document subscriptions for P2P replication")]
    List(P2pAccessArgs),
    #[command(about = "Subscribe documents for P2P replication")]
    Add(P2pDocumentsMutateArgs),
    #[command(about = "Remove document subscriptions from P2P replication")]
    Remove(P2pDocumentsMutateArgs),
    #[command(about = "Fetch documents from connected peers")]
    Sync(P2pDocumentsSyncArgs),
}

#[derive(clap::Args)]
pub(crate) struct P2pCollectionsMutateArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "collection", value_name = "COLLECTION")]
    pub(crate) collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    pub(crate) profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
pub(crate) struct P2pSyncBranchableArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "collection-id", value_name = "COLLECTION_ID")]
    pub(crate) collection_id: String,
}

#[derive(clap::Args)]
pub(crate) struct P2pSyncVersionsArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "version-id", value_name = "VERSION_ID")]
    pub(crate) version_ids: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pReplicatorAddArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) peer: String,
    #[arg(long = "collection", value_name = "COLLECTION")]
    pub(crate) collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    pub(crate) profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
pub(crate) struct P2pReplicatorRemoveArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) peer: String,
    #[arg(long = "collection", value_name = "COLLECTION")]
    pub(crate) collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    pub(crate) profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
pub(crate) struct P2pPairArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    /// Multiaddr of the remote peer (e.g. /ip4/1.2.3.4/tcp/4001/p2p/<peer-id>)
    #[arg(long)]
    pub(crate) peer: String,
    /// Collection profile to subscribe and replicate (default: chat-requests)
    #[arg(
        long = "profile",
        value_enum,
        value_name = "PROFILE",
        default_value = "chat-requests"
    )]
    pub(crate) profile: P2pCollectionProfileArg,
}

#[derive(clap::Args)]
pub(crate) struct P2pDocumentsMutateArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "doc-id", value_name = "DOC_ID")]
    pub(crate) doc_ids: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pDocumentsSyncArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_name = "COLLECTION")]
    pub(crate) collection: String,
    #[arg(long = "doc-id", value_name = "DOC_ID")]
    pub(crate) doc_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum P2pCollectionProfileArg {
    Runtime,
    Agent,
    DesktopConfig,
    ChatRequests,
    ToolServices,
}

#[derive(Subcommand)]
pub(crate) enum RequestCommand {
    #[command(
        about = "Create an AgentRequest document and optionally wait for the final AgentResponse"
    )]
    Submit(RequestSubmitArgs),
    #[command(about = "Show a stored AgentRequest document")]
    Show(RequestShowArgs),
    #[command(about = "Signal interrupt on an in-flight request (idempotent latch)")]
    Interrupt(RequestInterruptArgs),
    #[command(about = "Resend a stale-terminal request with a fresh TTL")]
    Resend(RequestResendArgs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum RequestInterruptCauseArg {
    #[value(name = "interrupted")]
    Interrupted,
    #[value(name = "deadline")]
    Deadline,
    #[value(name = "userCancelled")]
    UserCancelled,
}

impl From<RequestInterruptCauseArg> for defra_agent::tool_call_lifecycle::CancelCause {
    fn from(value: RequestInterruptCauseArg) -> Self {
        match value {
            RequestInterruptCauseArg::Interrupted => Self::Interrupted,
            RequestInterruptCauseArg::Deadline => Self::Deadline,
            RequestInterruptCauseArg::UserCancelled => Self::UserCancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum RequestInterruptOutputFormat {
    Text,
    Json,
}

#[derive(clap::Args)]
pub(crate) struct RequestSubmitArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
    #[arg(long)]
    pub(crate) content: Option<String>,
    #[arg(long = "content-file")]
    pub(crate) content_file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) session_id: Option<String>,
    #[arg(long)]
    pub(crate) behavior_id: Option<String>,
    #[arg(long)]
    pub(crate) temperature: Option<f64>,
    #[arg(long)]
    pub(crate) top_p: Option<f64>,
    #[arg(long)]
    pub(crate) top_k: Option<i64>,
    #[arg(long)]
    pub(crate) max_tokens: Option<i64>,
    #[arg(long)]
    pub(crate) metadata: Option<String>,
    #[arg(
        long = "valid-until",
        help = "TTL for this request (e.g. 30s, 5m, 2h, 1d). Default: 5m. Use \"none\" or 0 to disable."
    )]
    pub(crate) valid_until: Option<String>,
    #[arg(long = "output-file")]
    pub(crate) output_file: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub(crate) no_wait: bool,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}

#[derive(clap::Args)]
pub(crate) struct RequestInterruptArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(
        long,
        value_enum,
        default_value_t = RequestInterruptCauseArg::UserCancelled,
        help = "Reason for the interrupt: userCancelled for operator action, deadline for timeout-driven cancellation, interrupted for propagated runtime interruption"
    )]
    pub(crate) cause: RequestInterruptCauseArg,
    #[arg(long, default_value_t = false)]
    pub(crate) wait: bool,
    #[arg(
        long,
        value_name = "DURATION",
        default_value = "30s",
        help = "Maximum time to wait for a terminal request state when --wait is set"
    )]
    pub(crate) timeout: String,
    #[arg(
        long,
        value_enum,
        default_value_t = RequestInterruptOutputFormat::Text,
        help = "Output format; use json for scripts"
    )]
    pub(crate) output: RequestInterruptOutputFormat,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct RequestResendArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
    #[arg(long = "output-file")]
    pub(crate) output_file: Option<PathBuf>,
    #[arg(long, default_value_t = true)]
    pub(crate) no_wait: bool,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}

#[derive(clap::Args)]
pub(crate) struct RequestShowArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "output", value_enum, default_value_t = RequestShowOutputFormat::Text)]
    pub(crate) output: RequestShowOutputFormat,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RequestShowOutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
pub(crate) enum SubagentCommand {
    #[command(
        name = "list",
        about = "List subagent dispatch lineage",
        after_help = SUBAGENT_LIST_AFTER_HELP
    )]
    List(SubagentListArgs),
    #[command(about = "Cancel a subagent request and optionally cascade to linked children")]
    Cancel(SubagentCancelArgs),
}

#[derive(clap::Args)]
pub(crate) struct SubagentListArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_name = "REQUEST_ID")]
    pub(crate) root: Option<String>,
    #[arg(long, value_name = "N")]
    pub(crate) depth: Option<usize>,
    #[arg(long, value_enum, default_value_t = SubagentListOutput::Tree)]
    pub(crate) output: SubagentListOutput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum SubagentListOutput {
    Tree,
    Table,
    Json,
}

#[derive(clap::Args)]
pub(crate) struct SubagentCancelArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
    #[arg(
        long,
        default_value_t = true,
        default_missing_value = "true",
        num_args = 0..=1,
        action = ArgAction::Set,
        help = "Cancel linked subagent bridge tool-calls and interrupt linked child requests when their cancel policy allows it"
    )]
    pub(crate) cascade: bool,
    #[arg(
        long,
        default_value = "userCancelled",
        help = "CancelCause vocabulary value included in output and persisted for local bridge lifecycle cancellations: interrupted, deadline, or userCancelled"
    )]
    pub(crate) cause: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Wait until affected requests are terminal"
    )]
    pub(crate) wait: bool,
    #[arg(
        long,
        value_name = "DURATION",
        help = "Wait timeout such as 30s, 5m, or 1h. Only valid with --wait"
    )]
    pub(crate) timeout: Option<String>,
    #[arg(long, value_enum, default_value_t = SubagentCancelOutput::Text)]
    pub(crate) output: SubagentCancelOutput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum SubagentCancelOutput {
    Text,
    Json,
}

#[derive(Subcommand)]
pub(crate) enum SessionCommand {
    #[command(about = "Fork an existing session at a user-turn boundary")]
    Fork(SessionForkArgs),
}

#[derive(clap::Args)]
pub(crate) struct SessionForkArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "GraphQL endpoint for forking through a running runtime instead of opening local state"
    )]
    pub(crate) graphql: Option<String>,
    #[arg(
        long,
        help = "Override the caller agent DID (defaults to local identity)"
    )]
    pub(crate) agent_did: Option<String>,
    #[arg(long, value_name = "SOURCE_SESSION_ID")]
    pub(crate) from: String,
    #[arg(
        long,
        value_name = "N",
        help = "0-based user-turn index; fork cuts before this user message"
    )]
    pub(crate) at_user_turn: u32,
    #[arg(
        long,
        help = "Target behavior_id for the child; omit to inherit the parent's behavior"
    )]
    pub(crate) behavior: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum ResponseCommand {
    #[command(about = "Show the latest AgentResponse for a request")]
    Show(ResponseShowArgs),
    #[command(about = "Wait until a request reaches a terminal AgentResponse")]
    Wait(ResponseWaitArgs),
}

#[derive(clap::Args)]
pub(crate) struct ResponseShowArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ResponseWaitArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse_tools_set(extra: &[&str]) -> ToolSelectionUpsertArgs {
        let mut argv = vec![
            "defra-agent",
            "config",
            "tools",
            "set",
            "--graphql",
            "http://127.0.0.1/api/v0/graphql",
            "--agent-did",
            "did:key:z-test",
            "--selection-id",
            "s1",
        ];
        argv.extend_from_slice(extra);
        let cli = Cli::try_parse_from(argv).expect("config tools set should parse");
        match cli.command {
            Command::Config {
                command:
                    ConfigCommand::Tools {
                        command: ToolSelectionCommand::Set(args),
                    },
            } => args,
            _ => panic!("expected `config tools set`"),
        }
    }

    fn parse_init(extra: &[&str]) -> InitArgs {
        let mut argv = vec!["defra-agent", "init"];
        argv.extend_from_slice(extra);
        let cli = Cli::try_parse_from(argv).expect("init should parse");
        match cli.command {
            Command::Init(args) => args,
            _ => panic!("expected `init`"),
        }
    }

    #[test]
    fn enable_defra_query_flag_accepts_false_true_and_omission() {
        // Omitted -> None, so an unrelated `config tools set` preserves the
        // existing document setting rather than re-enabling it.
        assert_eq!(parse_tools_set(&[]).enable_defra_query, None);
        // Operators can actually disable it.
        assert_eq!(
            parse_tools_set(&["--enable-defra-query", "false"]).enable_defra_query,
            Some(false)
        );
        assert_eq!(
            parse_tools_set(&["--enable-defra-query", "true"]).enable_defra_query,
            Some(true)
        );
    }

    #[test]
    fn enable_memory_flag_accepts_false_true_and_omission() {
        assert_eq!(parse_tools_set(&[]).enable_memory, None);
        assert_eq!(
            parse_tools_set(&["--enable-memory", "false"]).enable_memory,
            Some(false)
        );
        assert_eq!(
            parse_tools_set(&["--enable-memory", "true"]).enable_memory,
            Some(true)
        );
    }

    #[test]
    fn enable_session_history_tool_flag_accepts_false_true_and_omission() {
        assert_eq!(parse_tools_set(&[]).enable_session_history_tool, None);
        assert_eq!(
            parse_tools_set(&["--enable-session-history-tool", "false"])
                .enable_session_history_tool,
            Some(false)
        );
        assert_eq!(
            parse_tools_set(&["--enable-session-history-tool", "true"]).enable_session_history_tool,
            Some(true)
        );
    }

    #[test]
    fn init_tool_audit_flags_parse() {
        let args = parse_init(&[
            "--enable-memory",
            "--disable-defra-query",
            "--tool-package",
            "write",
        ]);
        assert!(args.enable_memory);
        assert!(args.disable_defra_query);
        assert_eq!(args.tool_package, Some(ToolPackageArg::Write));

        let scoped = parse_init(&[
            "--defra-query-collection",
            "AgentRequest",
            "--defra-query-collection",
            "AgentResponse",
        ]);
        assert_eq!(
            scoped.defra_query_collections,
            vec!["AgentRequest".to_string(), "AgentResponse".to_string()]
        );
    }

    #[test]
    fn subagent_tool_flags_preserve_when_omitted_and_parse_when_present() {
        let omitted = parse_tools_set(&[]);
        assert_eq!(omitted.enable_file_tools, None);
        assert_eq!(omitted.enable_bash, None);
        assert_eq!(omitted.enable_meta_tools, None);
        assert!(omitted.subagent_targets.is_empty());
        assert!(!omitted.clear_subagent_targets);
        assert_eq!(omitted.subagent_spawn_enabled, None);
        assert_eq!(omitted.subagent_steering_enabled, None);
        assert_eq!(omitted.subagent_background_enabled, None);
        assert_eq!(omitted.subagent_allow_cross_deployment, None);
        assert_eq!(omitted.cross_deployment_spawn_timeout_seconds, None);

        let configured = parse_tools_set(&[
            "--subagent-target",
            r#"{"name":"worker","agent_did":"did:key:z-test","behavior_id":"worker","description":"worker"}"#,
            "--subagent-spawn-enabled",
            "true",
            "--subagent-steering-enabled",
            "true",
            "--subagent-background-enabled",
            "false",
            "--subagent-allow-cross-deployment",
            "true",
            "--cross-deployment-spawn-timeout-seconds",
            "90",
        ]);
        assert_eq!(configured.subagent_targets.len(), 1);
        assert_eq!(configured.subagent_spawn_enabled, Some(true));
        assert_eq!(configured.subagent_steering_enabled, Some(true));
        assert_eq!(configured.subagent_background_enabled, Some(false));
        assert_eq!(configured.subagent_allow_cross_deployment, Some(true));
        assert_eq!(configured.cross_deployment_spawn_timeout_seconds, Some(90));
    }

    #[test]
    fn legacy_tool_bool_flags_are_patch_optional() {
        let bare = parse_tools_set(&["--enable-file-tools", "--enable-bash"]);
        assert_eq!(bare.enable_file_tools, Some(true));
        assert_eq!(bare.enable_bash, Some(true));

        let explicit = parse_tools_set(&[
            "--enable-file-tools",
            "false",
            "--enable-bash",
            "false",
            "--enable-meta-tools",
            "false",
        ]);
        assert_eq!(explicit.enable_file_tools, Some(false));
        assert_eq!(explicit.enable_bash, Some(false));
        assert_eq!(explicit.enable_meta_tools, Some(false));
    }
}
