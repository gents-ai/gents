// Soft-cap justified: clap type definitions are a tightly-coupled unit.
// Splitting by subcommand would fragment the command tree declaration.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use defra_agent::BackendProviderKind;
use serde::{Deserialize, Serialize};

use crate::{
    CHAT_AFTER_HELP, CLI_AFTER_HELP, CONFIG_AFTER_HELP, CONFIG_EXPORT_AFTER_HELP,
    CONFIG_IMPORT_AFTER_HELP, DEFAULT_INIT_ENDPOINT, DIAGNOSE_AFTER_HELP, INIT_AFTER_HELP,
    P2P_AFTER_HELP, REQUEST_AFTER_HELP, RESET_AFTER_HELP, RESPONSE_AFTER_HELP, SERVER_AFTER_HELP,
    SHOW_AFTER_HELP, STATUS_AFTER_HELP,
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
    #[command(about = "Initialize a local agent home directory", after_help = INIT_AFTER_HELP)]
    Init(InitArgs),
    #[command(about = "Clear persisted local runtime state", after_help = RESET_AFTER_HELP)]
    Reset(ResetArgs),
    #[command(
        name = "server",
        about = "Run the local defra-agent runtime from an initialized home",
        after_help = SERVER_AFTER_HELP
    )]
    Server(ServeArgs),
    #[command(about = "Chat with the local agent in the terminal", after_help = CHAT_AFTER_HELP)]
    Chat(ChatArgs),
    #[command(about = "Inspect and control live P2P runtime connectivity", after_help = P2P_AFTER_HELP)]
    P2p {
        #[command(subcommand)]
        command: P2pCommand,
    },
    #[command(about = "Show stored runtime, request, or response state", after_help = SHOW_AFTER_HELP)]
    Show {
        #[command(subcommand)]
        command: ShowCommand,
    },
    #[command(about = "Show the current local runtime status", after_help = STATUS_AFTER_HELP)]
    Status(StatusArgs),
    #[command(about = "Run local configuration and runtime diagnostics", after_help = DIAGNOSE_AFTER_HELP)]
    Diagnose(DiagnoseArgs),
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
    #[arg(long, default_value = crate::DEFAULT_AGENT_NAME, help = "Local agent name. This becomes did:defra-agent:<AGENT_NAME>")]
    pub(crate) agent_name: String,
    #[arg(long)]
    pub(crate) key_path: Option<PathBuf>,
    #[arg(
        value_name = "INFERENCE_ENDPOINT",
        help = "Inference backend base URL, usually including /v1. Falls back to INFERENCE_ENDPOINT, then local Ollama."
    )]
    pub(crate) inference_endpoint: Option<String>,
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
        help = "Root directory for local file/bash tools. Defaults to the current working directory"
    )]
    pub(crate) tool_root: Option<PathBuf>,
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
        help = "Root directory for readonly/readwrite tool ceilings. Readonly defaults to the current working directory when unset"
    )]
    pub(crate) tool_root: Option<PathBuf>,
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
    #[arg(long, default_value_t = 300)]
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

#[derive(clap::Args)]
pub(crate) struct StatusArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
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
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::LlamaCpp => "llama-cpp",
        }
    }

    pub(crate) fn provider_kind(self) -> BackendProviderKind {
        match self {
            Self::OpenRouter => BackendProviderKind::OpenRouter,
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
            Self::Ollama => Some(DEFAULT_INIT_ENDPOINT),
            Self::Vllm => Some("http://127.0.0.1:8000/v1"),
            Self::LlamaCpp => Some("http://127.0.0.1:8080/v1"),
        }
    }

    pub(crate) fn default_api_key_env_var(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::GenericOpenAiCompatible | Self::Ollama | Self::Vllm | Self::LlamaCpp => None,
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
    #[command(about = "Write a ScheduledTask document")]
    Task {
        #[command(subcommand)]
        command: ScheduledTaskCommand,
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
    pub(crate) enable_file_tools: bool,
    #[arg(long)]
    pub(crate) file_tools_mode: Option<String>,
    #[arg(
        long,
        help = "Optional per-behavior file-tool root; relative paths resolve from the daemon cwd and must stay within any node-level tool root"
    )]
    pub(crate) file_tool_root: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub(crate) enable_bash: bool,
    #[arg(long)]
    pub(crate) bash_mode: Option<String>,
    #[arg(long = "cli-tool-name")]
    pub(crate) cli_tool_names: Vec<String>,
    #[arg(long, default_value_t = true)]
    pub(crate) enable_meta_tools: bool,
    #[arg(long = "delegate-to")]
    pub(crate) delegate_to: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum InferenceProfileCommand {
    #[command(name = "set")]
    Set(InferenceProfileUpsertArgs),
}

#[derive(Subcommand)]
pub(crate) enum ScheduledTaskCommand {
    #[command(name = "set")]
    Set(ScheduledTaskSetArgs),
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
pub(crate) struct ScheduledTaskSetArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) prompt: Option<String>,
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) behavior_id: Option<String>,
    #[arg(long)]
    pub(crate) interval_secs: i64,
    #[arg(long, default_value_t = true)]
    pub(crate) enabled: bool,
    #[arg(long)]
    pub(crate) next_run_at: Option<String>,
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
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) agent_did: Option<String>,
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
        help = "JSON export file to import. Reads stdin when omitted"
    )]
    pub(crate) path: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct ConfigValidateArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct ConfigDiffArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ConfigApplyArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
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
    #[arg(long = "output-file")]
    pub(crate) output_file: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub(crate) no_wait: bool,
    #[arg(long, default_value_t = 300)]
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
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
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
    #[arg(long, default_value_t = 300)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}
