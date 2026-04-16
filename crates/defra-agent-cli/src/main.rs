use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::{Parser, Subcommand, ValueEnum};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    cli_tool, default_behavior_id_for_agent, discover_backend_models,
    ensure_config_bootstrap_schemas, ensure_runtime_schemas, load_agent_behavior,
    load_agent_principal, upsert_agent_principal, AgentBehavior, AgentIdentity,
    BackendProviderKind, BashMode, DefraAgent, DocumentRuntimeOptions, FileToolMode, McpPool,
    ProcessLifecycleObserver, ProcessLifecycleState, SimpleIdentity, ToolCeiling,
    ToolSelectionDocument,
};
use p2p::iroh::parse_public_peer_addr;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::watch;

mod desired_state;
mod telemetry;
mod tui;

const DEFAULT_AGENT_NAME: &str = "default";
const DEFAULT_INIT_ENDPOINT: &str = "http://localhost:11434/v1";
const DEFAULT_INIT_MODEL_NAME: &str = "gemma4-26b-a4b";
const DEFAULT_HTTP_PORT: u16 = 9191;
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const SERVICE_NAME: &str = "defra-agent";
const SERVICE_BINARY: &str = "defra-agent";
const DEFAULT_P2P_MAX_CONCURRENT_DAG_FETCHES: usize = 4;
const DEFAULT_P2P_MAX_CONCURRENT_PUSH_TASKS: usize = 8;
const DEFAULT_P2P_RATE_LIMIT_BURST: u32 = 500;
const DEFAULT_P2P_RATE_LIMIT_RATE: f64 = 50.0;
const DEFAULT_LOG_FILTER: &str = concat!(
    "warn,",
    "defra_agent::agent::runtime=info,",
    "defra_agent::agent::daemon=info,",
    "defra_agent::agent::reconcile=info,",
    "defra_agent::hook=info,",
    "defra_agent::session::sessions=info,",
    "defra_agent::streaming=info,",
    "defra_agent::scheduler::loop_impl=info"
);
const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
const CLI_AFTER_HELP: &str = "\
Quick start:
  defra-agent init
  defra-agent server
  defra-agent chat

Inspect the local runtime:
  defra-agent status
  defra-agent show runtime
  defra-agent show response REQUEST_ID
  defra-agent reset

Update runtime documents:
  defra-agent config backend set ...
  defra-agent config behavior set ...
  defra-agent config tools set ...";
const INIT_AFTER_HELP: &str = "\
Bootstrap a local home directory with one default backend, one default behavior, and a safe read-only tool selection.

Examples:
  defra-agent init
  defra-agent init http://HOST:PORT/v1 --model-name MODEL
  defra-agent init --backend-preset openrouter --model-name MODEL
  defra-agent init --backend-preset openai --model-name MODEL
  defra-agent init $INFERENCE_ENDPOINT --model-name MODEL --write-tools

Next:
  ollama pull gemma4-26b-a4b
  defra-agent server
  defra-agent chat";
const RESET_AFTER_HELP: &str = "\
Examples:
  defra-agent reset
  defra-agent reset --home /path/to/home";
const SERVER_AFTER_HELP: &str = "\
`server` reads the initialized home directory, starts the embedded DefraDB runtime, serves GraphQL locally, and starts IROH P2P for desktop pairing.

Common flow:
  defra-agent init
  defra-agent server
  defra-agent-desktop init
  defra-agent-desktop
  defra-agent chat";
const CHAT_AFTER_HELP: &str = "\
Examples:
  defra-agent chat
  defra-agent chat \"summarize this repo\"
  defra-agent chat --session-id SESSION_ID \"continue the previous conversation\"

Diagnostics:
  defra-agent status
  defra-agent show response REQUEST_ID";
const P2P_AFTER_HELP: &str = "\
Examples:
  defra-agent p2p status
  defra-agent p2p peers --home /path/to/home
  defra-agent p2p connect --graphql http://127.0.0.1:9191/api/v0/graphql --peer <peer-id-or-address>
  defra-agent p2p collections add --profile chat-requests
  defra-agent p2p collections sync-versions --version-id <collection-version-id>
  defra-agent p2p replicators add --peer <peer-id-or-address> --profile runtime
  defra-agent p2p documents sync --collection AgentRequest --doc-id <doc-id>
  defra-agent p2p diagnose";
const STATUS_AFTER_HELP: &str = "\
Status reads the local runtime by default.

Examples:
  defra-agent status
  defra-agent status --home /path/to/home
  defra-agent status --graphql http://127.0.0.1:9191/api/v0/graphql";
const SHOW_AFTER_HELP: &str = "\
Examples:
  defra-agent show runtime
  defra-agent show request REQUEST_ID
  defra-agent show response REQUEST_ID";
const CONFIG_AFTER_HELP: &str = "\
Examples:
  defra-agent config validate --root infra/agents/default
  defra-agent config diff --root infra/agents/default --home /path/to/home
  defra-agent config apply --root infra/agents/default --home /path/to/home
  defra-agent config backend set --graphql URL --backend-id default-backend --name default-backend --backend-preset openrouter --max-concurrent 2
  defra-agent config backend discover-models --backend-preset openrouter
  defra-agent config behavior set --graphql URL --agent-did did:defra-agent:default --backend-id default-backend --model-name MODEL
  defra-agent config tools set --graphql URL --agent-did did:defra-agent:default --selection-id did:defra-agent:default:default:tools --enable-file-tools";
const REQUEST_AFTER_HELP: &str = "\
`request` is the low-level document path. Most users should prefer `defra-agent chat`.

Examples:
  defra-agent request submit --content \"summarize this repo\"
  defra-agent request show REQUEST_ID";
const RESPONSE_AFTER_HELP: &str = "\
Examples:
  defra-agent response wait REQUEST_ID
  defra-agent response show REQUEST_ID";
const DIAGNOSE_AFTER_HELP: &str = "\
Examples:
  defra-agent diagnose
  defra-agent diagnose --home /path/to/home
  defra-agent diagnose --graphql http://127.0.0.1:9191/api/v0/graphql";
const CONFIG_EXPORT_AFTER_HELP: &str = "\
Exports the desired configuration documents for one agent principal.

Examples:
  defra-agent config export > agent-config.json
  defra-agent config export --agent-did did:defra-agent:default > agent-config.json";
const CONFIG_IMPORT_AFTER_HELP: &str = "\
Imports desired configuration documents.

Default behavior is insert-only and will fail if a document already exists.
Use --override to switch to upsert mode.

Examples:
  defra-agent config import agent-config.json
  cat agent-config.json | defra-agent config import
  defra-agent config import agent-config.json --override";
const CONFIG_EXPORT_FORMAT_V1: &str = "defra-agent-config/v1";
const CONFIG_EXPORT_FORMAT: &str = "defra-agent-config/v2";
const STANDARD_READONLY_SYSTEM_PROMPT: &str = r#"You are a terminal-native engineering and operations agent running for the user inside a local DefraDB runtime.

Your job is to help with software work, debugging, codebase inspection, incident triage, release checks, infrastructure investigation, and general computer operations tasks. Build your conclusions from real evidence: inspect files, logs, command output, and tool results before making claims.

Work like a strong command-line operator:
- be concise and factual
- prefer direct answers over long essays
- explain what you found, not what you assume
- propose the next command, file, or check when it helps

You are currently in a read-only operating mode for local tools. You can inspect local state, but you cannot modify files or perform write-capable shell actions. If the user asks for a change, say clearly that the current tool mode is read-only and describe the exact edit or command you would apply if write access were enabled."#;
const STANDARD_READWRITE_SYSTEM_PROMPT: &str = r#"You are a terminal-native engineering and operations agent running for the user inside a local DefraDB runtime.

Your job is to help with software work, debugging, code changes, codebase maintenance, incident triage, release checks, infrastructure investigation, and general computer operations tasks. Build your conclusions from real evidence: inspect files, logs, command output, and tool results before making claims.

Work like a strong command-line operator:
- inspect first, then act
- keep changes focused and easy to explain
- prefer direct answers over long essays
- summarize exactly what changed and why
- avoid broad or risky operations unless the user clearly wants them

You have write-capable local tools. When the user asks you to make a change, you may edit files and use write-capable shell actions deliberately. Read the relevant state first, make the smallest effective change, and report the concrete outcome."#;
const SCHEMA_COLLECTION_CHECKS: &[(&str, &str)] = &[
    ("AgentPrincipal", "agent_did"),
    ("AgentBehavior", "behavior_id"),
    ("AgentRuntime", "agent_did"),
    ("ToolSelection", "selection_id"),
    ("InferenceProfile", "profile_id"),
    ("InferenceBackend", "backend_id"),
    ("AgentConversation", "session_id"),
    ("AgentRequest", "request_id"),
    ("AgentResponse", "request_id"),
    ("AgentToolResult", "agent_did"),
    ("AgentSession", "session_id"),
    ("AgentMessage", "message_key"),
    ("AgentToolCall", "tool_call_key"),
    ("CompactionEntry", "compaction_key"),
    ("ScheduledTask", "task_id"),
    ("ToolServiceRegistry", "service_id"),
];
const CONFIG_SCHEMA_COLLECTIONS: &[&str] = &[
    "AgentPrincipal",
    "AgentBehavior",
    "ToolSelection",
    "InferenceBackend",
];
const P2P_AGENT_COLLECTIONS: &[&str] = &[
    "AgentPrincipal",
    "AgentBehavior",
    "AgentRuntime",
    "ToolSelection",
    "InferenceBackend",
    "InferenceProfile",
];
const P2P_DESKTOP_CONFIG_COLLECTIONS: &[&str] = &[
    "AgentPrincipal",
    "AgentBehavior",
    "ToolSelection",
    "InferenceBackend",
    "InferenceProfile",
    "ToolServiceRegistry",
    "ScheduledTask",
];
const P2P_CHAT_REQUEST_COLLECTIONS: &[&str] = &[
    "AgentConversation",
    "AgentRequest",
    "AgentResponse",
    "AgentToolResult",
    "AgentSession",
    "AgentMessage",
    "AgentToolCall",
    "CompactionEntry",
];
const P2P_TOOL_SERVICE_COLLECTIONS: &[&str] = &["ToolServiceRegistry"];
const EXPORT_AGENT_PRINCIPAL_FIELDS: &str =
    "agent_did display_name default_behavior_id enabled created_at created_by";
const EXPORT_AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled created_at";
const EXPORT_TOOL_SELECTION_FIELDS: &str = "selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode cli_tool_names enable_meta_tools delegate_to";
const EXPORT_INFERENCE_BACKEND_FIELDS: &str =
    "backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status";
const EXPORT_INFERENCE_PROFILE_FIELDS: &str =
    "profile_id display_name context_window max_output_tokens max_turns temperature stream_batch_ms deadline_duration_secs";
const EXPORT_TOOL_SERVICE_REGISTRY_FIELDS: &str =
    "service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path";
const EXPORT_SCHEDULED_TASK_FIELDS: &str =
    "task_id agent_did behavior_id name prompt interval_secs enabled";

struct CliReadyObserver {
    tx: watch::Sender<ProcessLifecycleState>,
}

impl ProcessLifecycleObserver for CliReadyObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        let _ = self.tx.send(state);
    }
}

#[derive(Parser)]
#[command(
    name = "defra-agent",
    about = "Local-first CLI for bootstrapping, running, and inspecting a defra-agent runtime",
    after_help = CLI_AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    #[command(about = "Experimental terminal UI", hide = true)]
    Tui(TuiArgs),
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
struct InitArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    home: Option<PathBuf>,
    #[arg(long, hide = true)]
    data_dir: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete the existing home directory before re-initializing it"
    )]
    dangerously_overwrite: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Clear persisted local runtime state after initialization"
    )]
    reset: bool,
    #[arg(long, default_value = DEFAULT_AGENT_NAME, help = "Local agent name. This becomes did:defra-agent:<AGENT_NAME>")]
    agent_name: String,
    #[arg(long)]
    key_path: Option<PathBuf>,
    #[arg(
        value_name = "INFERENCE_ENDPOINT",
        help = "Inference backend base URL, usually including /v1. Falls back to INFERENCE_ENDPOINT, then local Ollama."
    )]
    inference_endpoint: Option<String>,
    #[arg(
        long,
        help = "Optional backend document id. Defaults to <agent-name>-backend"
    )]
    backend_id: Option<String>,
    #[arg(
        long,
        help = "Optional backend display name. Defaults to the backend id"
    )]
    backend_name: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Backend preset with provider/auth defaults for common local and hosted backends"
    )]
    backend_preset: Option<BackendPresetArg>,
    #[arg(
        long,
        help = "Backend provider kind. OpenAiCompatible covers OpenAI-style local and hosted endpoints"
    )]
    provider_kind: Option<String>,
    #[arg(long, help = "Raw API key stored directly in the backend document")]
    api_key: Option<String>,
    #[arg(long, help = "Environment variable name holding the backend API key")]
    api_key_env_var: Option<String>,
    #[arg(
        long,
        default_value = DEFAULT_INIT_MODEL_NAME,
        help = "Model id to bind to the default behavior"
    )]
    model_name: String,
    #[arg(long, default_value_t = 2)]
    max_concurrent: i64,
    #[arg(long, default_value_t = default_backend_max_queue_depth())]
    max_queue_depth: i64,
    #[arg(
        long,
        default_value_t = false,
        help = "Bootstrap write-capable tools instead of the safe read-only default"
    )]
    write_tools: bool,
    #[arg(
        long,
        help = "Root directory for local file/bash tools. Defaults to the current working directory"
    )]
    tool_root: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ResetArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    home: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    home: Option<PathBuf>,
    #[arg(long, hide = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1")]
    http_addr: IpAddr,
    #[arg(long, default_value_t = DEFAULT_HTTP_PORT)]
    http_port: u16,
    #[arg(long)]
    agent_name: Option<String>,
    #[arg(long)]
    key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "Operator safety cap that clamps document tool selection at runtime"
    )]
    tool_ceiling: Option<ToolCeilingArg>,
    #[arg(long = "cli-tool")]
    cli_tools: Vec<String>,
    #[arg(
        long,
        help = "Root directory for readonly/readwrite tool ceilings. Readonly defaults to the current working directory when unset"
    )]
    tool_root: Option<PathBuf>,
    #[arg(long)]
    p2p_bind_addr: Option<IpAddr>,
    #[arg(long)]
    p2p_port: Option<u16>,
    #[arg(long)]
    p2p_secret_key_path: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = P2pRelayModeArg::Disabled)]
    p2p_relay_mode: P2pRelayModeArg,
    #[arg(long, value_enum, default_value_t = P2pDiscoveryArg::Disabled)]
    p2p_discovery: P2pDiscoveryArg,
}

#[derive(clap::Args)]
struct ChatArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
    #[arg(long)]
    agent_name: Option<String>,
    #[arg(
        long,
        help = "Continue an existing session instead of starting a fresh one"
    )]
    session_id: Option<String>,
    #[arg(long, help = "Override the behavior for this one-off turn or session")]
    behavior_id: Option<String>,
    #[arg(long = "message-file", help = "Read the user message from a file")]
    message_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = ChatOutputFormat::Text)]
    output_format: ChatOutputFormat,
    #[arg(long = "output-file", help = "Write the final response to a file")]
    output_file: Option<PathBuf>,
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    poll_secs: u64,
    #[arg(value_name = "MESSAGE")]
    message: Vec<String>,
}

#[derive(clap::Args, Clone)]
struct TuiArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
    #[arg(long)]
    agent_name: Option<String>,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    behavior_id: Option<String>,
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
    #[arg(long, default_value_t = 500)]
    poll_ms: u64,
}

#[derive(Subcommand)]
enum ShowCommand {
    #[command(about = "Show a stored AgentRequest document")]
    Request(RequestShowArgs),
    #[command(about = "Show the latest AgentResponse for a request")]
    Response(ResponseShowArgs),
    #[command(about = "Show the persisted AgentRuntime document")]
    Runtime(RuntimeShowArgs),
}

#[derive(clap::Args)]
struct StatusArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ChatOutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum P2pTransportArg {
    None,
    Iroh,
}

impl P2pTransportArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Iroh => "iroh",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum P2pRelayModeArg {
    Default,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum P2pDiscoveryArg {
    #[value(name = "n0")]
    N0,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum BackendPresetArg {
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

#[derive(clap::Args)]
struct DiagnoseArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
}

#[derive(clap::Args)]
struct RuntimeShowArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
}

#[derive(Subcommand)]
enum ConfigCommand {
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
enum ToolCeilingArg {
    MetaOnly,
    Readonly,
    Readwrite,
}

impl BackendPresetArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::GenericOpenAiCompatible => "generic-openai-compatible",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::LlamaCpp => "llama-cpp",
        }
    }

    fn provider_kind(self) -> BackendProviderKind {
        match self {
            Self::OpenRouter => BackendProviderKind::OpenRouter,
            Self::GenericOpenAiCompatible
            | Self::OpenAi
            | Self::Ollama
            | Self::Vllm
            | Self::LlamaCpp => BackendProviderKind::OpenAiCompatible,
        }
    }

    fn default_endpoint(self) -> Option<&'static str> {
        match self {
            Self::GenericOpenAiCompatible => None,
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::Ollama => Some(DEFAULT_INIT_ENDPOINT),
            Self::Vllm => Some("http://127.0.0.1:8000/v1"),
            Self::LlamaCpp => Some("http://127.0.0.1:8080/v1"),
        }
    }

    fn default_api_key_env_var(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::GenericOpenAiCompatible | Self::Ollama | Self::Vllm | Self::LlamaCpp => None,
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedBackendConfig {
    provider_kind: BackendProviderKind,
    endpoint: String,
    api_key: Option<String>,
    api_key_env_var: Option<String>,
}

#[derive(Debug, Clone)]
struct DiscoveredBackendTarget {
    backend_id: Option<String>,
    preset: Option<BackendPresetArg>,
    provider_kind: BackendProviderKind,
    endpoint: String,
    api_key: Option<String>,
    api_key_env_var: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct InitSummary {
    backend_id: String,
    backend_name: String,
    provider_kind: BackendProviderKind,
    endpoint: String,
    api_key: Option<String>,
    api_key_env_var: Option<String>,
    model_name: String,
    max_concurrent: i64,
    max_queue_depth: i64,
    default_behavior_id: String,
    tool_selection_id: String,
    tool_ceiling: ToolCeilingArg,
    tool_root: Option<String>,
    created_principal: bool,
    created_default_behavior: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredInitConfig {
    /// Filesystem-only bootstrap context. Runtime configuration lives in DefraDB
    /// documents; these fields let later CLI commands find the local key and
    /// operator tool ceiling without asking for flags on every run.
    home: String,
    agent_name: String,
    agent_did: String,
    key_path: Option<String>,
    tool_ceiling: ToolCeilingArg,
    tool_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRuntimeState {
    home: String,
    graphql: String,
    agent_name: String,
    agent_did: String,
    default_behavior_id: String,
    #[serde(default = "default_p2p_transport")]
    p2p_transport: String,
    #[serde(default)]
    p2p_peer_id: Option<String>,
    #[serde(default)]
    p2p_listen_addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct VersionResponse {
    service: &'static str,
    binary: &'static str,
    package: &'static str,
    version: &'static str,
    repository: &'static str,
    build: BuildMetadata,
}

#[derive(Debug, Clone, Serialize)]
struct BuildMetadata {
    git_sha: Option<&'static str>,
    git_ref: Option<&'static str>,
    git_dirty: Option<bool>,
    target: Option<&'static str>,
    profile: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
struct NodeIdentityResponse {
    #[serde(rename = "PeerID")]
    peer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct P2pShareableAddressResponse {
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct P2pPeerRow {
    id: String,
}

#[derive(Debug, Clone, Serialize)]
struct P2pCollectionSubscriptionRow {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct P2pReplicatorRow {
    #[serde(rename = "ID", default)]
    id: Option<String>,
    #[serde(rename = "Addresses", default)]
    addresses: Vec<String>,
    #[serde(rename = "CollectionIDs", default)]
    collection_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct P2pReplicatorOutputRow {
    id: Option<String>,
    addresses: Vec<String>,
    collection_ids: Vec<String>,
    collection_names: Vec<String>,
}

#[derive(Debug, Serialize)]
struct P2pReplicatorRequest {
    #[serde(rename = "Collections")]
    collections: Vec<String>,
    #[serde(rename = "Addresses")]
    addresses: Vec<String>,
}

#[derive(Debug, Serialize)]
struct P2pReplicatorDeleteRequest {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Collections")]
    collections: Vec<String>,
}

#[derive(Debug, Serialize)]
struct P2pSyncDocumentsRequest {
    #[serde(rename = "collectionName")]
    collection_name: String,
    #[serde(rename = "docIDs")]
    doc_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct P2pSyncBranchableRequest {
    #[serde(rename = "collectionID")]
    collection_id: String,
}

#[derive(Debug, Serialize)]
struct P2pSyncVersionsRequest {
    #[serde(rename = "versionIDs")]
    version_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigExportBundle {
    format: String,
    agent_did: String,
    exported_at: String,
    access_mode: String,
    agent_principal: Option<Value>,
    #[serde(default)]
    agent_behaviors: Vec<Value>,
    #[serde(default)]
    tool_selections: Vec<Value>,
    #[serde(default)]
    inference_backends: Vec<Value>,
    #[serde(default)]
    inference_profiles: Vec<Value>,
    #[serde(default)]
    tool_service_registries: Vec<Value>,
    #[serde(default)]
    scheduled_tasks: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigApplyCounts {
    agent_principal: usize,
    agent_behaviors: usize,
    tool_selections: usize,
    inference_backends: usize,
    inference_profiles: usize,
    tool_service_registries: usize,
    scheduled_tasks: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigApplyReport {
    status: &'static str,
    ok: bool,
    exact_match: bool,
    changed: bool,
    root: String,
    access_mode: String,
    agent_did: String,
    planned: desired_state::DesiredStateDiffCollectionsCounts,
    applied: ConfigApplyCounts,
    remaining: desired_state::DesiredStateDiffCollectionsCounts,
}

#[derive(Clone)]
struct RuntimeHttpState {
    graphql: String,
    started_at: String,
    started_instant: Instant,
}

#[derive(Debug, Deserialize, Serialize)]
struct MetricsQueryData {
    #[serde(rename = "AgentRuntime", default)]
    agent_runtimes: Vec<MetricsRuntimeRow>,
    #[serde(rename = "InferenceBackend", default)]
    inference_backends: Vec<MetricsBackendRow>,
}

#[derive(Debug, Deserialize, Serialize)]
struct MetricsRuntimeRow {
    agent_did: String,
    #[serde(default)]
    process_state: String,
    #[serde(default)]
    reconcile_phase: String,
    #[serde(default)]
    active_generation: i64,
    #[serde(default)]
    router_generation: i64,
    #[serde(default)]
    runnable_behavior_count: i64,
    #[serde(default)]
    unavailable_behavior_count: i64,
    #[serde(default)]
    last_reconcile_result: String,
    #[serde(default)]
    last_reconcile_completed_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct MetricsBackendRow {
    backend_id: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    max_concurrent: i64,
    #[serde(default)]
    max_queue_depth: i64,
    #[serde(default)]
    probe_status: String,
    last_probe: Option<String>,
}

enum ConfigAccess {
    Graphql(String),
    Local(EmbeddedNode),
}

impl ConfigAccess {
    fn mode(&self) -> &'static str {
        match self {
            Self::Graphql(_) => "graphql",
            Self::Local(_) => "local",
        }
    }

    async fn execute(&self, query: &str) -> Result<Value> {
        match self {
            Self::Graphql(graphql) => post_graphql(graphql, query).await,
            Self::Local(node) => {
                let response = node.execute(query).await;
                if response.has_errors() {
                    anyhow::bail!("graphql returned errors: {:?}", response.errors);
                }
                Ok(json!({
                    "data": response.data.unwrap_or(Value::Null),
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ExistingDocumentRef {
    doc_id: String,
    deleted: bool,
}

#[derive(Debug, Clone)]
struct InferenceBackendUpsertDocument {
    backend_id: String,
    name: String,
    provider_kind: BackendProviderKind,
    endpoint: String,
    api_key: Option<String>,
    api_key_env_var: Option<String>,
    max_concurrent: i64,
    max_queue_depth: i64,
    enabled: bool,
    models_on_add: Vec<String>,
    models_on_update: Option<Vec<String>>,
    probe_status: String,
}

async fn write_inference_backend_document(
    access: &ConfigAccess,
    backend: &InferenceBackendUpsertDocument,
) -> Result<String> {
    let models_add = string_list_field("models", &backend.models_on_add)
        .ok_or_else(|| anyhow::anyhow!("backend models field could not be rendered"))?;
    let models_update = backend
        .models_on_update
        .as_ref()
        .and_then(|models| string_list_field("models", models));
    let update_fields = vec![
        Some(format!(
            r#"name: "{}""#,
            escape_graphql_string(&backend.name)
        )),
        Some(format!(
            r#"provider_kind: "{}""#,
            escape_graphql_string(backend.provider_kind.as_str())
        )),
        Some(format!(
            r#"endpoint: "{}""#,
            escape_graphql_string(&backend.endpoint)
        )),
        Some(nullable_string_field("api_key", backend.api_key.as_deref())),
        Some(nullable_string_field(
            "api_key_env_var",
            backend.api_key_env_var.as_deref(),
        )),
        Some(format!("max_concurrent: {}", backend.max_concurrent)),
        Some(format!("max_queue_depth: {}", backend.max_queue_depth)),
        Some(format!(
            "enabled: {}",
            graphql_bool_literal(backend.enabled)
        )),
        models_update,
        Some(format!(
            r#"probe_status: "{}""#,
            escape_graphql_string(&backend.probe_status)
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }},
                add: {{
                    backend_id: "{backend_id}",
                    name: "{name}",
                    provider_kind: "{provider_kind}",
                    endpoint: "{endpoint}",
                    {api_key},
                    {api_key_env_var},
                    max_concurrent: {max_concurrent},
                    max_queue_depth: {max_queue_depth},
                    enabled: {enabled},
                    {models_add},
                    probe_status: "{probe_status}"
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        backend_id = escape_graphql_string(&backend.backend_id),
        name = escape_graphql_string(&backend.name),
        provider_kind = escape_graphql_string(backend.provider_kind.as_str()),
        endpoint = escape_graphql_string(&backend.endpoint),
        api_key = nullable_string_field("api_key", backend.api_key.as_deref()),
        api_key_env_var =
            nullable_string_field("api_key_env_var", backend.api_key_env_var.as_deref()),
        max_concurrent = backend.max_concurrent,
        max_queue_depth = backend.max_queue_depth,
        enabled = graphql_bool_literal(backend.enabled),
        models_add = models_add,
        probe_status = escape_graphql_string(&backend.probe_status),
        update_fields = update_fields,
    );
    let response = access.execute(&mutation).await?;
    extract_mutation_doc_id(&response, "InferenceBackend")
}

async fn write_agent_behavior_document(
    access: &ConfigAccess,
    behavior: &AgentBehavior,
) -> Result<String> {
    let created_at = behavior
        .created_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let add_fields = vec![
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(&behavior.behavior_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&behavior.agent_did)
        )),
        optional_string_field("display_name", behavior.display_name.as_deref()),
        optional_string_field("system_prompt", behavior.system_prompt.as_deref()),
        optional_string_field("backend_id", behavior.backend_id.as_deref()),
        optional_string_field("model_name", behavior.model_name.as_deref()),
        optional_string_field("tool_selection_id", behavior.tool_selection_id.as_deref()),
        optional_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        optional_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        optional_f64_field("compaction_threshold", behavior.compaction_threshold),
        Some(format!(
            "enabled: {}",
            graphql_bool_literal(behavior.enabled)
        )),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let update_fields = vec![
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&behavior.agent_did)
        )),
        optional_string_field("display_name", behavior.display_name.as_deref()),
        optional_string_field("system_prompt", behavior.system_prompt.as_deref()),
        optional_string_field("backend_id", behavior.backend_id.as_deref()),
        optional_string_field("model_name", behavior.model_name.as_deref()),
        optional_string_field("tool_selection_id", behavior.tool_selection_id.as_deref()),
        optional_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        optional_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        optional_f64_field("compaction_threshold", behavior.compaction_threshold),
        Some(format!(
            "enabled: {}",
            graphql_bool_literal(behavior.enabled)
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let mutation = format!(
        r#"mutation {{
            upsert_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        behavior_id = escape_graphql_string(&behavior.behavior_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = access.execute(&mutation).await?;
    extract_mutation_doc_id(&response, "AgentBehavior")
}

async fn write_tool_selection_document(
    access: &ConfigAccess,
    selection: &ToolSelectionDocument,
) -> Result<String> {
    let add_fields = tool_selection_fields(selection, true);
    let update_fields = tool_selection_fields(selection, false);
    let mutation = format!(
        r#"mutation {{
            upsert_ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        selection_id = escape_graphql_string(&selection.selection_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = access.execute(&mutation).await?;
    extract_mutation_doc_id(&response, "ToolSelection")
}

fn tool_selection_fields(selection: &ToolSelectionDocument, include_id: bool) -> String {
    let mut fields = Vec::new();
    if include_id {
        fields.push(format!(
            r#"selection_id: "{}""#,
            escape_graphql_string(&selection.selection_id)
        ));
    }
    fields.push(format!(
        r#"agent_did: "{}""#,
        escape_graphql_string(&selection.agent_did)
    ));
    fields.extend(
        [
            optional_string_field("display_name", selection.display_name.as_deref()),
            optional_bool_field("enable_file_tools", selection.enable_file_tools),
            optional_string_field("file_tools_mode", selection.file_tools_mode.as_deref()),
            Some(nullable_string_field(
                "file_tool_root",
                selection.file_tool_root.as_deref(),
            )),
            optional_bool_field("enable_bash", selection.enable_bash),
            optional_string_field("bash_mode", selection.bash_mode.as_deref()),
            selection
                .cli_tool_names
                .as_ref()
                .and_then(|values| string_list_field("cli_tool_names", values)),
            optional_bool_field("enable_meta_tools", selection.enable_meta_tools),
            selection
                .delegate_to
                .as_ref()
                .and_then(|values| string_list_field("delegate_to", values)),
        ]
        .into_iter()
        .flatten(),
    );
    fields.join(",\n                    ")
}

async fn query_documents_by_unique_value(
    access: &ConfigAccess,
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
    show_deleted: bool,
) -> Result<Vec<ExistingDocumentRef>> {
    let show_deleted_arg = if show_deleted {
        "showDeleted: true, "
    } else {
        ""
    };
    let query = format!(
        r#"{{
            {collection_name}(
                {show_deleted_arg}filter: {{ {unique_field}: {{ _eq: "{unique_value}" }} }},
                limit: 16
            ) {{
                _docID
                _deleted
            }}
        }}"#,
        collection_name = collection_name,
        show_deleted_arg = show_deleted_arg,
        unique_field = unique_field,
        unique_value = escape_graphql_string(unique_value),
    );
    let response = access.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    rows.into_iter()
        .map(|row| {
            Ok(ExistingDocumentRef {
                doc_id: row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{collection_name} lookup row missing _docID for {unique_field}={unique_value}: {row}"
                        )
                    })?
                    .to_string(),
                deleted: row.get("_deleted").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

fn select_existing_document(
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
    rows: &[ExistingDocumentRef],
) -> Result<Option<ExistingDocumentRef>> {
    let live_rows = rows.iter().filter(|row| !row.deleted).collect::<Vec<_>>();
    if live_rows.len() > 1 {
        anyhow::bail!(
            "multiple live {collection_name} documents share {unique_field}={unique_value}"
        );
    }
    if let Some(row) = live_rows.first() {
        return Ok(Some((*row).clone()));
    }

    let deleted_rows = rows.iter().filter(|row| row.deleted).collect::<Vec<_>>();
    if deleted_rows.len() > 1 {
        anyhow::bail!(
            "multiple deleted {collection_name} tombstones share {unique_field}={unique_value}"
        );
    }

    Ok(deleted_rows.first().map(|row| (*row).clone()))
}

async fn write_scheduled_task_document(
    access: &ConfigAccess,
    task_id: &str,
    add_doc: &Value,
    update_doc: &Value,
) -> Result<String> {
    let existing = select_existing_document(
        "ScheduledTask",
        "task_id",
        task_id,
        &query_documents_by_unique_value(access, "ScheduledTask", "task_id", task_id, true).await?,
    )?;

    let Some(existing) = existing.as_ref() else {
        return create_scheduled_task_document(access, task_id, add_doc).await;
    };
    if existing.deleted {
        return create_scheduled_task_document(access, task_id, add_doc).await;
    }

    let input_literal = graphql_input_literal(update_doc)?;
    let mutation = format!(
        r#"mutation {{
            update_ScheduledTask(docID: "{doc_id}", input: {input_literal}) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(&existing.doc_id),
        input_literal = input_literal,
    );

    let response = access.execute(&mutation).await?;
    match extract_mutation_doc_id(&response, "ScheduledTask") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_scheduled_task_row(access, task_id, update_doc).await?;
            if let Some(row) = current {
                let current_doc_id = row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let deleted = row
                    .get("_deleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !deleted
                    && current_doc_id == existing.doc_id
                    && scheduled_task_row_matches_expected(&row, update_doc)?
                {
                    return Ok(current_doc_id);
                }
                return Err(anyhow::anyhow!(
                    "{}\nScheduledTask post-update row did not converge for task_id {}: {}",
                    extract_error,
                    task_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nScheduledTask task_id {} has no row after update attempt",
                extract_error,
                task_id
            ))
        }
    }
}

async fn create_scheduled_task_document(
    access: &ConfigAccess,
    task_id: &str,
    add_doc: &Value,
) -> Result<String> {
    let input_literal = graphql_input_literal(add_doc)?;
    let mutation = format!(
        r#"mutation {{
            create_ScheduledTask(input: {input_literal}) {{ _docID }}
        }}"#,
        input_literal = input_literal,
    );
    let response = access.execute(&mutation).await?;
    match extract_mutation_doc_id(&response, "ScheduledTask") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_scheduled_task_row(access, task_id, add_doc).await?;
            if let Some(row) = current {
                let deleted = row
                    .get("_deleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !deleted && scheduled_task_row_matches_expected(&row, add_doc)? {
                    return row
                        .get("_docID")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "ScheduledTask live row missing _docID after recreate: {}",
                                row
                            )
                        });
                }
                return Err(anyhow::anyhow!(
                    "{}\nScheduledTask post-create row did not converge for task_id {}: {}",
                    extract_error,
                    task_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nScheduledTask task_id {} has no live row after create attempt",
                extract_error,
                task_id
            ))
        }
    }
}

async fn select_matching_scheduled_task_row(
    access: &ConfigAccess,
    task_id: &str,
    expected: &Value,
) -> Result<Option<Value>> {
    let rows = query_scheduled_task_rows(access, task_id, true).await?;
    let live_rows = rows
        .into_iter()
        .filter(|row| row.get("_deleted").and_then(Value::as_bool) != Some(true))
        .collect::<Vec<_>>();
    if live_rows.len() > 1 {
        anyhow::bail!(
            "multiple live ScheduledTask rows share task_id {} during post-write verification",
            task_id
        );
    }
    if let Some(row) = live_rows.into_iter().next() {
        if scheduled_task_row_matches_expected(&row, expected)? {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

async fn query_scheduled_task_rows(
    access: &ConfigAccess,
    task_id: &str,
    show_deleted: bool,
) -> Result<Vec<Value>> {
    let show_deleted_arg = if show_deleted {
        "showDeleted: true, "
    } else {
        ""
    };
    let query = format!(
        r#"{{
            ScheduledTask(
                {show_deleted_arg}filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                limit: 4
            ) {{
                _docID
                _deleted
                task_id
                agent_did
                behavior_id
                name
                prompt
                interval_secs
                enabled
                next_run_at
                last_run_at
                last_status
                last_error
                run_count
                created_at
                updated_at
            }}
        }}"#,
        show_deleted_arg = show_deleted_arg,
        task_id = escape_graphql_string(task_id),
    );
    let response = access.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("ScheduledTask"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn scheduled_task_row_matches_expected(row: &Value, expected: &Value) -> Result<bool> {
    let expected = expected
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("ScheduledTask expected document must be an object"))?;
    let actual = row
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("ScheduledTask row must be an object"))?;
    Ok(expected
        .iter()
        .all(|(key, value)| actual.get(key).is_some_and(|actual| actual == value)))
}

fn runtime_contract_router(graphql: String) -> Router {
    let state = RuntimeHttpState {
        graphql,
        started_at: chrono::Utc::now().to_rfc3339(),
        started_instant: Instant::now(),
    };

    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/version", get(version_handler))
        .route("/healthz", get(healthz_handler))
        .with_state(state)
}

async fn metrics_handler(State(state): State<RuntimeHttpState>) -> Response {
    match render_prometheus_metrics(&state.graphql).await {
        Ok(body) => ([(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)], body).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("metrics render failed: {error}"),
        )
            .into_response(),
    }
}

async fn version_handler() -> impl IntoResponse {
    axum::Json(version_response())
}

async fn healthz_handler(State(state): State<RuntimeHttpState>) -> Response {
    match load_metrics_query_data(&state.graphql).await {
        Ok(data) => {
            let health = render_healthz_payload(&state, Some(&data), None);
            let status = if health.get("ok") == Some(&Value::Bool(true)) {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (status, axum::Json(health)).into_response()
        }
        Err(error) => {
            let health = render_healthz_payload(&state, None, Some(error.to_string()));
            (StatusCode::SERVICE_UNAVAILABLE, axum::Json(health)).into_response()
        }
    }
}

fn version_response() -> VersionResponse {
    VersionResponse {
        service: SERVICE_NAME,
        binary: SERVICE_BINARY,
        package: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        repository: env!("CARGO_PKG_REPOSITORY"),
        build: BuildMetadata {
            git_sha: option_env!("DEFRA_AGENT_BUILD_GIT_SHA"),
            git_ref: option_env!("DEFRA_AGENT_BUILD_GIT_REF"),
            git_dirty: option_env!("DEFRA_AGENT_BUILD_GIT_DIRTY").and_then(|value| match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }),
            target: option_env!("DEFRA_AGENT_BUILD_TARGET"),
            profile: option_env!("DEFRA_AGENT_BUILD_PROFILE"),
        },
    }
}

fn render_healthz_payload(
    state: &RuntimeHttpState,
    data: Option<&MetricsQueryData>,
    error: Option<String>,
) -> Value {
    let version = version_response();
    let uptime_seconds = state.started_instant.elapsed().as_secs();

    match data {
        Some(data) => {
            let runtime_ready = data
                .agent_runtimes
                .iter()
                .any(|runtime| runtime.process_state == ProcessLifecycleState::Ready.as_str());
            let runtime_degraded = data
                .agent_runtimes
                .iter()
                .any(|runtime| runtime.unavailable_behavior_count > 0);
            let backend_degraded = data
                .inference_backends
                .iter()
                .any(|backend| backend.enabled && backend.probe_status != "healthy");
            let ok = runtime_ready;
            let status = if !runtime_ready {
                "unhealthy"
            } else if runtime_degraded || backend_degraded {
                "degraded"
            } else {
                "ok"
            };
            let runtime_status = if runtime_ready {
                if runtime_degraded {
                    "degraded"
                } else {
                    "ok"
                }
            } else {
                "unhealthy"
            };
            let backend_status = if backend_degraded { "degraded" } else { "ok" };

            json!({
                "status": status,
                "ok": ok,
                "service": SERVICE_NAME,
                "version": version.version,
                "started_at": state.started_at,
                "uptime_seconds": uptime_seconds,
                "checks": {
                    "http": {
                        "status": "ok",
                    },
                    "graphql": {
                        "status": "ok",
                        "endpoint": state.graphql,
                    },
                    "runtime": {
                        "status": runtime_status,
                        "ready": runtime_ready,
                        "count": data.agent_runtimes.len(),
                    },
                    "backends": {
                        "status": backend_status,
                        "count": data.inference_backends.len(),
                    },
                },
                "runtimes": data.agent_runtimes,
                "backends": data.inference_backends,
            })
        }
        None => json!({
            "status": "unhealthy",
            "ok": false,
            "service": SERVICE_NAME,
            "version": version.version,
            "started_at": state.started_at,
            "uptime_seconds": uptime_seconds,
            "checks": {
                "http": {
                    "status": "ok",
                },
                "graphql": {
                    "status": "unhealthy",
                    "endpoint": state.graphql,
                    "error": error.unwrap_or_else(|| "runtime GraphQL status unavailable".to_string()),
                },
                "runtime": {
                    "status": "unknown",
                    "ready": false,
                    "count": 0,
                },
                "backends": {
                    "status": "unknown",
                    "count": 0,
                },
            },
            "runtimes": [],
            "backends": [],
        }),
    }
}

async fn render_prometheus_metrics(graphql: &str) -> Result<String> {
    let data = load_metrics_query_data(graphql).await?;

    let mut lines = Vec::new();
    push_metric_prelude(
        &mut lines,
        "defra_agent_up",
        "Whether the defra-agent process is serving.",
    );
    push_metric_sample(&mut lines, "defra_agent_up", &[], 1);

    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_process_state",
        "One-hot process lifecycle state for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_reconcile_phase",
        "One-hot reconcile phase for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_last_reconcile_result",
        "One-hot last reconcile result for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_active_generation",
        "Current active runtime generation.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_router_generation",
        "Current router-observed runtime generation.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_runnable_behaviors",
        "Number of runnable behaviors in the active runtime snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_unavailable_behaviors",
        "Number of unavailable behaviors in the active runtime snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_last_reconcile_completed_at_seconds",
        "Unix timestamp of the last completed reconcile.",
    );

    for runtime in &data.agent_runtimes {
        let agent_did = runtime.agent_did.clone();
        for state in [
            "uninitialized",
            "recovering",
            "ready",
            "shuttingDown",
            "shutdown",
        ] {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_process_state",
                &[
                    ("agent_did", agent_did.clone()),
                    ("state", state.to_string()),
                ],
                i64::from(runtime.process_state == state),
            );
        }
        for phase in ["idle", "debouncing", "resolving", "diffing", "applying"] {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_reconcile_phase",
                &[
                    ("agent_did", agent_did.clone()),
                    ("phase", phase.to_string()),
                ],
                i64::from(runtime.reconcile_phase == phase),
            );
        }
        for result in ["startup", "noop", "applied", "error"] {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_last_reconcile_result",
                &[
                    ("agent_did", agent_did.clone()),
                    ("result", result.to_string()),
                ],
                i64::from(runtime.last_reconcile_result == result),
            );
        }
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_active_generation",
            &[("agent_did", agent_did.clone())],
            runtime.active_generation,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_router_generation",
            &[("agent_did", agent_did.clone())],
            runtime.router_generation,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_runnable_behaviors",
            &[("agent_did", agent_did.clone())],
            runtime.runnable_behavior_count,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_unavailable_behaviors",
            &[("agent_did", agent_did.clone())],
            runtime.unavailable_behavior_count,
        );
        if let Some(timestamp) = rfc3339_timestamp_seconds(&runtime.last_reconcile_completed_at) {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_last_reconcile_completed_at_seconds",
                &[("agent_did", agent_did)],
                timestamp,
            );
        }
    }

    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_enabled",
        "Whether an inference backend is enabled.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_max_concurrent",
        "Configured maximum concurrency for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_max_queue_depth",
        "Configured admission queue depth for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_probe_status",
        "Current probe status for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_last_probe_seconds",
        "Unix timestamp of the last backend probe.",
    );

    for backend in &data.inference_backends {
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_enabled",
            &[("backend_id", backend.backend_id.clone())],
            i64::from(backend.enabled),
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_max_concurrent",
            &[("backend_id", backend.backend_id.clone())],
            backend.max_concurrent,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_max_queue_depth",
            &[("backend_id", backend.backend_id.clone())],
            backend.max_queue_depth,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_probe_status",
            &[
                ("backend_id", backend.backend_id.clone()),
                ("status", backend.probe_status.clone()),
            ],
            1,
        );
        if let Some(timestamp) = backend
            .last_probe
            .as_deref()
            .and_then(rfc3339_timestamp_seconds)
        {
            push_metric_sample(
                &mut lines,
                "defra_agent_backend_last_probe_seconds",
                &[("backend_id", backend.backend_id.clone())],
                timestamp,
            );
        }
    }

    lines.push(String::new());
    Ok(lines.join("\n"))
}

async fn load_metrics_query_data(graphql: &str) -> Result<MetricsQueryData> {
    let response = post_graphql(
        graphql,
        r#"{
            AgentRuntime {
                agent_did
                process_state
                reconcile_phase
                active_generation
                router_generation
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
                last_reconcile_completed_at
            }
            InferenceBackend {
                backend_id
                enabled
                max_concurrent
                max_queue_depth
                probe_status
                last_probe
            }
        }"#,
    )
    .await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    serde_json::from_value(data).context("decoding runtime HTTP query response")
}

fn push_metric_prelude(lines: &mut Vec<String>, name: &str, help: &str) {
    lines.push(format!("# HELP {name} {help}"));
    lines.push(format!("# TYPE {name} gauge"));
}

fn push_metric_sample(
    lines: &mut Vec<String>,
    name: &str,
    labels: &[(&str, String)],
    value: impl std::fmt::Display,
) {
    lines.push(format!("{name}{} {value}", format_metric_labels(labels),));
}

fn format_metric_labels(labels: &[(&str, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let rendered = labels
        .iter()
        .map(|(key, value)| format!(r#"{key}="{}""#, escape_prometheus_label(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{rendered}}}")
}

fn escape_prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn rfc3339_timestamp_seconds(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}

#[derive(Subcommand)]
enum BackendCommand {
    #[command(name = "set")]
    Set(BackendUpsertArgs),
    #[command(name = "discover-models")]
    DiscoverModels(BackendDiscoverModelsArgs),
}

#[derive(Subcommand)]
enum BehaviorCommand {
    #[command(name = "set")]
    Set(BehaviorUpsertArgs),
}

#[derive(Subcommand)]
enum ToolSelectionCommand {
    #[command(name = "set")]
    Set(ToolSelectionUpsertArgs),
}

#[derive(clap::Args)]
struct BehaviorUpsertArgs {
    #[arg(long)]
    graphql: String,
    #[arg(long)]
    agent_did: String,
    #[arg(long)]
    behavior_id: Option<String>,
    #[arg(long)]
    display_name: Option<String>,
    #[arg(long)]
    system_prompt_file: Option<PathBuf>,
    #[arg(long)]
    backend_id: Option<String>,
    #[arg(long)]
    model_name: Option<String>,
    #[arg(long)]
    tool_selection_id: Option<String>,
    #[arg(long)]
    inference_profile_id: Option<String>,
    #[arg(long)]
    compaction_strategy: Option<String>,
    #[arg(long)]
    compaction_threshold: Option<f64>,
    #[arg(long, default_value_t = true)]
    enabled: bool,
}

#[derive(clap::Args)]
struct ToolSelectionUpsertArgs {
    #[arg(long)]
    graphql: String,
    #[arg(long)]
    agent_did: String,
    #[arg(long)]
    selection_id: String,
    #[arg(long)]
    display_name: Option<String>,
    #[arg(long, default_value_t = false)]
    enable_file_tools: bool,
    #[arg(long)]
    file_tools_mode: Option<String>,
    #[arg(
        long,
        help = "Optional per-behavior file-tool root; relative paths resolve from the daemon cwd and must stay within any node-level tool root"
    )]
    file_tool_root: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    enable_bash: bool,
    #[arg(long)]
    bash_mode: Option<String>,
    #[arg(long = "cli-tool-name")]
    cli_tool_names: Vec<String>,
    #[arg(long, default_value_t = true)]
    enable_meta_tools: bool,
    #[arg(long = "delegate-to")]
    delegate_to: Vec<String>,
}

#[derive(Subcommand)]
enum InferenceProfileCommand {
    #[command(name = "set")]
    Set(InferenceProfileUpsertArgs),
}

#[derive(Subcommand)]
enum ScheduledTaskCommand {
    #[command(name = "set")]
    Set(ScheduledTaskSetArgs),
}

#[derive(clap::Args)]
struct InferenceProfileUpsertArgs {
    #[arg(long)]
    graphql: String,
    #[arg(long)]
    profile_id: String,
    #[arg(long)]
    display_name: Option<String>,
    #[arg(long)]
    context_window: Option<i64>,
    #[arg(long)]
    max_output_tokens: Option<i64>,
    #[arg(long)]
    max_turns: Option<i64>,
    #[arg(long)]
    temperature: Option<f64>,
    #[arg(long)]
    stream_batch_ms: Option<i64>,
    #[arg(long)]
    deadline_duration_secs: Option<i64>,
}

#[derive(clap::Args)]
struct ScheduledTaskSetArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
    #[arg(long)]
    task_id: String,
    #[arg(long)]
    name: String,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    prompt_file: Option<PathBuf>,
    #[arg(long)]
    behavior_id: Option<String>,
    #[arg(long)]
    interval_secs: i64,
    #[arg(long, default_value_t = true)]
    enabled: bool,
    #[arg(long)]
    next_run_at: Option<String>,
}

#[derive(clap::Args)]
struct BackendUpsertArgs {
    #[arg(long)]
    graphql: String,
    #[arg(long)]
    backend_id: String,
    #[arg(long)]
    name: String,
    #[arg(
        long,
        value_enum,
        help = "Backend preset with provider/auth defaults for common local and hosted backends"
    )]
    backend_preset: Option<BackendPresetArg>,
    #[arg(
        long,
        help = "Backend provider kind. OpenAiCompatible covers OpenAI-style local and hosted endpoints"
    )]
    provider_kind: Option<String>,
    #[arg(
        long,
        help = "Inference backend base URL, usually including /v1. Falls back to the preset default when available"
    )]
    endpoint: Option<String>,
    #[arg(long, help = "Raw API key stored directly in the backend document")]
    api_key: Option<String>,
    #[arg(
        long,
        help = "Environment variable name holding this backend's API key"
    )]
    api_key_env_var: Option<String>,
    #[arg(long)]
    max_concurrent: i64,
    #[arg(long, default_value_t = default_backend_max_queue_depth())]
    max_queue_depth: i64,
    #[arg(long, default_value_t = true)]
    enabled: bool,
    #[arg(long, default_value = "healthy")]
    probe_status: String,
}

#[derive(clap::Args)]
struct BackendDiscoverModelsArgs {
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    backend_id: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Backend preset with provider/auth defaults for common local and hosted backends"
    )]
    backend_preset: Option<BackendPresetArg>,
    #[arg(
        long,
        help = "Backend provider kind. OpenAiCompatible covers OpenAI-style local and hosted endpoints"
    )]
    provider_kind: Option<String>,
    #[arg(
        long,
        help = "Inference backend base URL, usually including /v1. Falls back to the preset default when available"
    )]
    endpoint: Option<String>,
    #[arg(long, help = "Raw API key to use for this probe only")]
    api_key: Option<String>,
    #[arg(long, help = "Environment variable name holding the probe API key")]
    api_key_env_var: Option<String>,
}

#[derive(clap::Args)]
struct ConfigExportArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
}

#[derive(clap::Args)]
struct ConfigImportArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(
        long = "override",
        default_value_t = false,
        help = "Upsert documents instead of failing when they already exist"
    )]
    override_existing: bool,
    #[arg(
        value_name = "PATH",
        help = "JSON export file to import. Reads stdin when omitted"
    )]
    path: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ConfigValidateArgs {
    #[arg(long, value_name = "ROOT")]
    root: PathBuf,
}

#[derive(clap::Args)]
struct ConfigDiffArgs {
    #[arg(long, value_name = "ROOT")]
    root: PathBuf,
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
}

#[derive(clap::Args)]
struct ConfigApplyArgs {
    #[arg(long, value_name = "ROOT")]
    root: PathBuf,
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
}

#[derive(Subcommand)]
enum P2pCommand {
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
struct P2pAccessArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
}

#[derive(clap::Args)]
struct P2pConnectArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    peer: String,
}

#[derive(Subcommand)]
enum P2pCollectionsCommand {
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
enum P2pReplicatorsCommand {
    #[command(about = "List configured P2P replicators")]
    List(P2pAccessArgs),
    #[command(about = "Configure a peer replicator for collections or profiles")]
    Add(P2pReplicatorAddArgs),
    #[command(about = "Remove a peer replicator for collections or profiles")]
    Remove(P2pReplicatorRemoveArgs),
}

#[derive(Subcommand)]
enum P2pDocumentsCommand {
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
struct P2pCollectionsMutateArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long = "collection", value_name = "COLLECTION")]
    collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
struct P2pSyncBranchableArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long = "collection-id", value_name = "COLLECTION_ID")]
    collection_id: String,
}

#[derive(clap::Args)]
struct P2pSyncVersionsArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long = "version-id", value_name = "VERSION_ID")]
    version_ids: Vec<String>,
}

#[derive(clap::Args)]
struct P2pReplicatorAddArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    peer: String,
    #[arg(long = "collection", value_name = "COLLECTION")]
    collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
struct P2pReplicatorRemoveArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    peer: String,
    #[arg(long = "collection", value_name = "COLLECTION")]
    collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
struct P2pDocumentsMutateArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long = "doc-id", value_name = "DOC_ID")]
    doc_ids: Vec<String>,
}

#[derive(clap::Args)]
struct P2pDocumentsSyncArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long, value_name = "COLLECTION")]
    collection: String,
    #[arg(long = "doc-id", value_name = "DOC_ID")]
    doc_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum P2pCollectionProfileArg {
    Runtime,
    Agent,
    DesktopConfig,
    ChatRequests,
    ToolServices,
}

#[derive(Subcommand)]
enum RequestCommand {
    #[command(
        about = "Create an AgentRequest document and optionally wait for the final AgentResponse"
    )]
    Submit(RequestSubmitArgs),
    #[command(about = "Show a stored AgentRequest document")]
    Show(RequestShowArgs),
}

#[derive(clap::Args)]
struct RequestSubmitArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long)]
    agent_did: Option<String>,
    #[arg(long)]
    content: Option<String>,
    #[arg(long = "content-file")]
    content_file: Option<PathBuf>,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    behavior_id: Option<String>,
    #[arg(long)]
    temperature: Option<f64>,
    #[arg(long)]
    top_p: Option<f64>,
    #[arg(long)]
    top_k: Option<i64>,
    #[arg(long)]
    max_tokens: Option<i64>,
    #[arg(long)]
    metadata: Option<String>,
    #[arg(long = "output-file")]
    output_file: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    no_wait: bool,
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    poll_secs: u64,
}

#[derive(clap::Args)]
struct RequestShowArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long = "request-id")]
    request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    request_id: Option<String>,
}

#[derive(Subcommand)]
enum ResponseCommand {
    #[command(about = "Show the latest AgentResponse for a request")]
    Show(ResponseShowArgs),
    #[command(about = "Wait until a request reaches a terminal AgentResponse")]
    Wait(ResponseWaitArgs),
}

#[derive(clap::Args)]
struct ResponseShowArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long = "request-id")]
    request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    request_id: Option<String>,
}

#[derive(clap::Args)]
struct ResponseWaitArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long)]
    graphql: Option<String>,
    #[arg(long = "request-id")]
    request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    request_id: Option<String>,
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    poll_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let telemetry = telemetry::init(DEFAULT_LOG_FILTER)?;
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init(args) => init(args).await,
        Command::Reset(args) => reset(args).await,
        Command::Server(args) => serve(args).await,
        Command::Chat(args) => chat(args).await,
        Command::P2p { command } => match command {
            P2pCommand::Status(args) => p2p_status(args).await,
            P2pCommand::Peers(args) => p2p_peers(args).await,
            P2pCommand::Connect(args) => p2p_connect(args).await,
            P2pCommand::Collections { command } => match command {
                P2pCollectionsCommand::List(args) => p2p_collections_list(args).await,
                P2pCollectionsCommand::Add(args) => p2p_collections_add(args).await,
                P2pCollectionsCommand::Remove(args) => p2p_collections_remove(args).await,
                P2pCollectionsCommand::SyncBranchable(args) => {
                    p2p_collections_sync_branchable(args).await
                }
                P2pCollectionsCommand::SyncVersions(args) => {
                    p2p_collections_sync_versions(args).await
                }
            },
            P2pCommand::Replicators { command } => match command {
                P2pReplicatorsCommand::List(args) => p2p_replicators_list(args).await,
                P2pReplicatorsCommand::Add(args) => p2p_replicators_add(args).await,
                P2pReplicatorsCommand::Remove(args) => p2p_replicators_remove(args).await,
            },
            P2pCommand::Documents { command } => match command {
                P2pDocumentsCommand::List(args) => p2p_documents_list(args).await,
                P2pDocumentsCommand::Add(args) => p2p_documents_add(args).await,
                P2pDocumentsCommand::Remove(args) => p2p_documents_remove(args).await,
                P2pDocumentsCommand::Sync(args) => p2p_documents_sync(args).await,
            },
            P2pCommand::Diagnose(args) => p2p_diagnose(args).await,
        },
        Command::Tui(args) => tui::run(args).await,
        Command::Show { command } => match command {
            ShowCommand::Request(args) => request_show(args).await,
            ShowCommand::Response(args) => response_show(args).await,
            ShowCommand::Runtime(args) => show_runtime(args).await,
        },
        Command::Status(args) => status(args).await,
        Command::Diagnose(args) => diagnose(args).await,
        Command::Config { command } => match command {
            ConfigCommand::Validate(args) => config_validate(args).await,
            ConfigCommand::Diff(args) => config_diff(args).await,
            ConfigCommand::Apply(args) => config_apply(args).await,
            ConfigCommand::Backend { command } => match command {
                BackendCommand::Set(args) => backend_set(args).await,
                BackendCommand::DiscoverModels(args) => backend_discover_models(args).await,
            },
            ConfigCommand::Behavior { command } => match command {
                BehaviorCommand::Set(args) => behavior_set(args).await,
            },
            ConfigCommand::Tools { command } => match command {
                ToolSelectionCommand::Set(args) => tool_selection_set(args).await,
            },
            ConfigCommand::Profile { command } => match command {
                InferenceProfileCommand::Set(args) => inference_profile_set(args).await,
            },
            ConfigCommand::Task { command } => match command {
                ScheduledTaskCommand::Set(args) => scheduled_task_set(args).await,
            },
            ConfigCommand::Export(args) => config_export(args).await,
            ConfigCommand::Import(args) => config_import(args).await,
        },
        Command::Request { command } => match command {
            RequestCommand::Submit(args) => request_submit(args).await,
            RequestCommand::Show(args) => request_show(args).await,
        },
        Command::Response { command } => match command {
            ResponseCommand::Show(args) => response_show(args).await,
            ResponseCommand::Wait(args) => response_wait(args).await,
        },
    };
    telemetry.shutdown();
    result
}

async fn init(args: InitArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    if args.dangerously_overwrite {
        dangerously_overwrite_home(&home_dir)?;
    }
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| default_data_dir(&home_dir));
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;

    let key_path = args
        .key_path
        .clone()
        .unwrap_or_else(|| default_key_path(&home_dir, &args.agent_name));
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating key directory {}", parent.display()))?;
    }

    let identity = Arc::new(SimpleIdentity::new(&args.agent_name, &key_path, None));
    identity
        .sign(b"defra-agent init identity")
        .await
        .context("creating or loading agent identity key")?;
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .build()
        .await
        .context("building embedded defra node for init")?;
    ensure_config_bootstrap_schemas(&node).await?;

    let access = ConfigAccess::Local(node);
    let summary = initialize_runtime_home(&access, &args, identity.did()).await?;
    let stored = StoredInitConfig {
        home: home_dir.to_string_lossy().to_string(),
        agent_name: args.agent_name.clone(),
        agent_did: identity.did().to_string(),
        key_path: Some(key_path.to_string_lossy().to_string()),
        tool_ceiling: summary.tool_ceiling,
        tool_root: summary.tool_root.clone(),
    };
    write_init_config(&home_dir, &stored)?;
    let runtime_state_reset = if args.reset {
        clear_runtime_state(&home_dir)?
    } else {
        false
    };

    let output = json!({
        "status": "initialized",
        "home": home_dir,
        "agent_name": args.agent_name,
        "agent_did": identity.did(),
        "key_path": key_path,
        "default_behavior_id": summary.default_behavior_id,
        "tool_selection_id": summary.tool_selection_id,
        "tool_ceiling": format_tool_ceiling(summary.tool_ceiling),
        "tool_root": summary.tool_root,
        "runtime_state_reset": runtime_state_reset,
        "identity": {
            "agent_did": identity.did(),
            "key_path": stored.key_path,
            "permission_boundary": "This DID and key identify the permission boundary for every action the agent runtime performs."
        },
        "next_steps": init_next_steps(&summary),
        "init": summary,
    });
    print_json(&output)?;

    Ok(())
}

async fn reset(args: ResetArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let runtime_state_path = runtime_state_path(&home_dir);
    let cleared = clear_runtime_state(&home_dir)?;
    let output = json!({
        "status": "reset",
        "home": home_dir,
        "runtime_state_path": runtime_state_path,
        "cleared": cleared,
    });
    print_json(&output)?;
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| default_data_dir(&home_dir));
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let http_addr = SocketAddr::new(args.http_addr, args.http_port);
    let graphql_url = format!(
        "http://{}:{}/api/v0/graphql",
        display_host(args.http_addr),
        args.http_port
    );
    let p2p_config = resolve_server_p2p_config(&home_dir, &args)?;
    let mut node_builder = EmbeddedNode::builder().data_path(&data_dir).with_http(
        defra_node::HttpConfig::with_addr(http_addr)
            .with_extra_routes(runtime_contract_router(graphql_url.clone())),
    );
    if let Some(config) = p2p_config {
        node_builder = node_builder.with_p2p(config);
    }
    let node = Arc::new(
        node_builder
            .build()
            .await
            .context("building embedded defra node")?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;
    let init_config = read_init_config(&home_dir)?;
    if let (Some(explicit), Some(config)) = (args.agent_name.as_deref(), init_config.as_ref()) {
        if explicit != config.agent_name {
            anyhow::bail!(
                "--agent-name {} does not match initialized home agent {}",
                explicit,
                config.agent_name
            );
        }
    }

    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let agent_name = args
        .agent_name
        .clone()
        .or_else(|| init_config.as_ref().map(|config| config.agent_name.clone()))
        .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
    let key_path = args
        .key_path
        .clone()
        .or_else(|| {
            init_config
                .as_ref()
                .and_then(|config| config.key_path.as_ref().map(PathBuf::from))
        })
        .unwrap_or_else(|| default_key_path(&home_dir, &agent_name));
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating key directory {}", parent.display()))?;
    }
    let effective_tool_ceiling = args
        .tool_ceiling
        .or_else(|| init_config.as_ref().map(|config| config.tool_ceiling))
        .unwrap_or(ToolCeilingArg::MetaOnly);
    let configured_tool_root = args.tool_root.clone().or_else(|| {
        init_config
            .as_ref()
            .and_then(|config| config.tool_root.as_ref().map(PathBuf::from))
    });
    let effective_tool_root = match effective_tool_ceiling {
        ToolCeilingArg::MetaOnly => configured_tool_root,
        ToolCeilingArg::Readonly => Some(match configured_tool_root {
            Some(root) => root,
            None => resolve_default_tool_root(None)?,
        }),
        ToolCeilingArg::Readwrite => Some(configured_tool_root.ok_or_else(|| {
            anyhow::anyhow!("--tool-root is required when --tool-ceiling readwrite")
        })?),
    };
    let mut tool_ceiling = match effective_tool_ceiling {
        ToolCeilingArg::MetaOnly => ToolCeiling::meta_only(),
        ToolCeilingArg::Readonly => ToolCeiling::readonly_at(
            effective_tool_root
                .as_ref()
                .expect("readonly root resolved"),
        ),
        ToolCeilingArg::Readwrite => ToolCeiling::readwrite(
            effective_tool_root
                .as_ref()
                .expect("readwrite root resolved"),
        ),
    };
    for cli_tool_arg in &args.cli_tools {
        tool_ceiling = tool_ceiling.with_cli_tool(parse_cli_tool_arg(cli_tool_arg)?);
    }
    let identity = Arc::new(SimpleIdentity::new(&agent_name, &key_path, None));
    let (ready_tx, mut ready_rx) = watch::channel(ProcessLifecycleState::Uninitialized);

    let agent = DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            mcp_pool: McpPool::new(),
            local_hostname: Some(local_hostname),
            tool_ceiling,
            process_state_observer: Some(Arc::new(CliReadyObserver { tx: ready_tx })),
            ..Default::default()
        },
    )
    .await
    .with_context(|| {
        format!(
            "starting defra-agent server from {}\n{}",
            home_dir.display(),
            server_start_failure_hint(&home_dir)
        )
    })?;
    let runnable_behaviors = agent
        .behaviors()
        .iter()
        .map(|behavior| {
            json!({
                "behavior_id": behavior.name,
                "backend_id": behavior.backend_id,
                "model_name": behavior.model_name,
            })
        })
        .collect::<Vec<_>>();
    let default_behavior_id = agent.default_behavior_id().to_string();
    let unavailable_behaviors = agent.unavailable_behaviors().clone();
    let behavior_readiness = if unavailable_behaviors.is_empty() {
        "ready"
    } else {
        "degraded"
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });

    let mut run_handle = tokio::spawn(agent.run(shutdown_rx));
    loop {
        if *ready_rx.borrow() == ProcessLifecycleState::Ready {
            break;
        }

        tokio::select! {
            changed = ready_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            joined = &mut run_handle => {
                let result = joined.context("joining defra-agent runtime task")?;
                return result;
            }
        }
    }

    let p2p_status = load_local_server_p2p_status(node.as_ref(), P2pTransportArg::Iroh).await?;
    write_runtime_state(
        &home_dir,
        &StoredRuntimeState {
            home: home_dir.to_string_lossy().to_string(),
            graphql: graphql_url.clone(),
            agent_name: agent_name.clone(),
            agent_did: identity.did().to_string(),
            default_behavior_id: default_behavior_id.clone(),
            p2p_transport: p2p_status
                .get("p2p_transport")
                .and_then(Value::as_str)
                .unwrap_or(P2pTransportArg::None.as_str())
                .to_string(),
            p2p_peer_id: p2p_status
                .get("p2p_peer_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            p2p_listen_addresses: p2p_status
                .get("p2p_listen_addresses")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        },
    )?;

    let output = json!({
        "status": "serving",
        "behavior_readiness": behavior_readiness,
        "home": home_dir,
        "agent_name": agent_name,
        "agent_did": identity.did(),
        "default_behavior_id": default_behavior_id,
        "tool_ceiling": format_tool_ceiling(effective_tool_ceiling),
        "tool_root": effective_tool_root,
        "runnable_behaviors": runnable_behaviors,
        "unavailable_behaviors": unavailable_behaviors,
        "graphql": graphql_url,
        "p2p_transport": p2p_status.get("p2p_transport").cloned().unwrap_or(Value::String(default_p2p_transport())),
        "p2p_peer_id": p2p_status.get("p2p_peer_id").cloned().unwrap_or(Value::Null),
        "p2p_listen_addresses": p2p_status.get("p2p_listen_addresses").cloned().unwrap_or_else(|| json!([])),
    });
    print_json(&output)?;
    eprintln!(
        "defra-agent server is running with IROH P2P. Press Ctrl-C to stop. For the desktop demo, run `defra-agent-desktop init`, launch `defra-agent-desktop`, wait for `replication: subscriptions armed`, then chat."
    );

    run_handle
        .await
        .context("joining defra-agent runtime task")?
}

fn default_p2p_transport() -> String {
    P2pTransportArg::Iroh.as_str().to_string()
}

fn default_p2p_secret_key_path(home_dir: &Path) -> PathBuf {
    home_dir.join("p2p-secret-key")
}

fn resolve_server_p2p_config(
    home_dir: &Path,
    args: &ServeArgs,
) -> Result<Option<defra_node::P2PConfig>> {
    let secret_key_path = args
        .p2p_secret_key_path
        .clone()
        .unwrap_or_else(|| default_p2p_secret_key_path(home_dir));
    if let Some(parent) = secret_key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating P2P key directory {}", parent.display()))?;
    }
    Ok(Some(defra_node::P2PConfig {
        port: args.p2p_port.unwrap_or(0),
        bind_addr: Some(
            args.p2p_bind_addr
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ),
        relay_mode: match args.p2p_relay_mode {
            P2pRelayModeArg::Default => p2p::iroh::IrohRelayModeConfig::Default,
            P2pRelayModeArg::Disabled => p2p::iroh::IrohRelayModeConfig::Disabled,
        },
        discovery: match args.p2p_discovery {
            P2pDiscoveryArg::N0 => p2p::iroh::IrohDiscoveryConfig::N0,
            P2pDiscoveryArg::Disabled => p2p::iroh::IrohDiscoveryConfig::Disabled,
        },
        secret_key_path: Some(secret_key_path),
        load_persisted_collections: true,
        max_concurrent_dag_fetches: DEFAULT_P2P_MAX_CONCURRENT_DAG_FETCHES,
        max_concurrent_push_tasks: DEFAULT_P2P_MAX_CONCURRENT_PUSH_TASKS,
        rate_limit_burst: DEFAULT_P2P_RATE_LIMIT_BURST,
        rate_limit_rate: DEFAULT_P2P_RATE_LIMIT_RATE,
    }))
}

async fn load_local_server_p2p_status(
    node: &EmbeddedNode,
    transport: P2pTransportArg,
) -> Result<Value> {
    match transport {
        P2pTransportArg::None => Ok(json!({
            "enabled": false,
            "p2p_transport": transport.as_str(),
            "p2p_peer_id": Value::Null,
            "p2p_listen_addresses": [],
            "p2p_connected_peers": [],
        })),
        P2pTransportArg::Iroh => {
            let p2p = node.p2p().ok_or_else(|| {
                anyhow::anyhow!(
                    "P2P transport was requested but is not available on the embedded node"
                )
            })?;
            let peer_id = p2p
                .local_peer_id()
                .await
                .context("loading local P2P peer id from the embedded node")?;
            let listen_addresses = wait_for_p2p_listen_addresses(p2p).await?;
            let connected_peers = p2p
                .connected_peers()
                .await
                .context("loading connected P2P peers from the embedded node")?;
            Ok(json!({
                "enabled": true,
                "p2p_transport": transport.as_str(),
                "p2p_peer_id": peer_id,
                "p2p_listen_addresses": listen_addresses,
                "p2p_connected_peers": connected_peers,
            }))
        }
    }
}

async fn wait_for_p2p_listen_addresses(
    p2p: &dyn defra_p2p_adapter::P2POperations,
) -> Result<Vec<String>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listen_addresses = p2p
            .listen_addresses()
            .await
            .context("loading local P2P listen addresses from the embedded node")?;
        if !listen_addresses.is_empty() {
            return Ok(listen_addresses);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(listen_addresses);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn chat(args: ChatArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let runtime_state = read_runtime_state(&home_dir)?;
    let init_config = read_init_config(&home_dir)?;
    let graphql = args
        .graphql
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.graphql.clone()))
        .unwrap_or_else(|| format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"));
    let agent_name = args
        .agent_name
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_name.clone()))
        .or_else(|| init_config.as_ref().map(|config| config.agent_name.clone()))
        .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
    let agent_did = args
        .agent_did
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_did.clone()))
        .or_else(|| init_config.as_ref().map(|config| config.agent_did.clone()))
        .unwrap_or_else(|| format!("did:defra-agent:{agent_name}"));
    let session_id = args
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Some(message) = resolve_chat_message(&args.message, args.message_file.as_deref())? {
        match args.output_format {
            ChatOutputFormat::Text => {
                let (_submitted, response) = submit_chat_turn(
                    &graphql,
                    &agent_did,
                    &session_id,
                    args.behavior_id.as_deref(),
                    &message,
                    args.timeout_secs,
                    args.poll_secs,
                )
                .await?;
                if let Some(path) = args.output_file.as_deref() {
                    write_text_output_file(path, response_text_content(&response))?;
                }
            }
            ChatOutputFormat::Json => {
                let output = submit_chat_turn_json(
                    &graphql,
                    &agent_did,
                    &session_id,
                    args.behavior_id.as_deref(),
                    &message,
                    args.timeout_secs,
                    args.poll_secs,
                )
                .await?;
                print_json(&output)?;
                if let Some(path) = args.output_file.as_deref() {
                    write_json_output_file(path, &output)?;
                }
            }
        }
        return Ok(());
    }

    if args.output_format != ChatOutputFormat::Text {
        anyhow::bail!("interactive chat only supports --output-format text");
    }
    if let Some(path) = args.output_file.as_deref() {
        anyhow::bail!(
            "--output-file {} requires a one-shot message via MESSAGE or --message-file",
            path.display()
        );
    }

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = io::stdout();
    loop {
        write!(stdout, "> ")?;
        stdout.flush()?;
        let Some(line) = lines.next() else {
            break;
        };
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "/exit" | "/quit" | "exit" | "quit") {
            break;
        }

        submit_chat_turn(
            &graphql,
            &agent_did,
            &session_id,
            args.behavior_id.as_deref(),
            trimmed,
            args.timeout_secs,
            args.poll_secs,
        )
        .await?;
    }

    Ok(())
}

async fn backend_set(args: BackendUpsertArgs) -> Result<()> {
    let backend = resolve_backend_upsert_config(&args)?;
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let doc = InferenceBackendUpsertDocument {
        backend_id: args.backend_id.clone(),
        name: args.name.clone(),
        provider_kind: backend.provider_kind,
        endpoint: backend.endpoint.clone(),
        api_key: backend.api_key.clone(),
        api_key_env_var: backend.api_key_env_var.clone(),
        max_concurrent: args.max_concurrent,
        max_queue_depth: args.max_queue_depth,
        enabled: args.enabled,
        models_on_add: vec!["default".to_string()],
        models_on_update: None,
        probe_status: args.probe_status.clone(),
    };
    let doc_id = write_inference_backend_document(&access, &doc).await?;
    let output = json!({
        "doc_id": doc_id,
        "backend_id": args.backend_id,
        "backend_preset": args.backend_preset.map(BackendPresetArg::as_str),
        "provider_kind": backend.provider_kind.as_str(),
        "endpoint": backend.endpoint,
        "api_key": backend.api_key.as_ref().map(|_| "<redacted>"),
        "api_key_env_var": backend.api_key_env_var,
        "max_concurrent": args.max_concurrent,
        "max_queue_depth": args.max_queue_depth,
        "enabled": args.enabled,
        "probe_status": args.probe_status,
    });
    print_json(&output)?;
    Ok(())
}

async fn backend_discover_models(args: BackendDiscoverModelsArgs) -> Result<()> {
    let target = resolve_backend_discovery_target(&args).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building backend discovery client")?;
    let discovered_models = discover_backend_models(
        &client,
        target.provider_kind,
        &target.endpoint,
        target.api_key.as_deref(),
    )
    .await?;

    let output = json!({
        "backend_id": target.backend_id,
        "backend_preset": target.preset.map(BackendPresetArg::as_str),
        "provider_kind": target.provider_kind.as_str(),
        "endpoint": target.endpoint,
        "api_key": target.api_key.as_ref().map(|_| "<redacted>"),
        "api_key_env_var": target.api_key_env_var,
        "discovered_models": discovered_models,
    });
    print_json(&output)?;
    Ok(())
}

async fn resolve_backend_discovery_target(
    args: &BackendDiscoverModelsArgs,
) -> Result<DiscoveredBackendTarget> {
    if let Some(backend_id) = normalize_optional_string(args.backend_id.as_deref()) {
        if args.graphql.is_none() {
            anyhow::bail!("--graphql is required when --backend-id is set");
        }
        if args.backend_preset.is_some()
            || normalize_optional_string(args.provider_kind.as_deref()).is_some()
            || normalize_optional_string(args.endpoint.as_deref()).is_some()
            || normalize_optional_string(args.api_key.as_deref()).is_some()
            || normalize_optional_string(args.api_key_env_var.as_deref()).is_some()
        {
            anyhow::bail!(
                "--backend-id uses the stored backend document; do not combine it with explicit preset, endpoint, provider, or auth flags"
            );
        }
        let backend = load_backend_row(
            args.graphql
                .as_deref()
                .expect("checked graphql when backend_id is set"),
            &backend_id,
        )
        .await?;
        let provider_kind = BackendProviderKind::parse_optional(
            backend.get("provider_kind").and_then(Value::as_str),
        )?;
        let endpoint = backend
            .get("endpoint")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("backend {backend_id} is missing endpoint"))?
            .to_string();
        let api_key = normalize_optional_string(backend.get("api_key").and_then(Value::as_str));
        let api_key_env_var =
            normalize_optional_string(backend.get("api_key_env_var").and_then(Value::as_str));
        if api_key.is_some() && api_key_env_var.is_some() {
            anyhow::bail!(
                "backend {backend_id} sets both raw api_key and api_key_env_var; discovery is ambiguous"
            );
        }
        let resolved_api_key = match (api_key, api_key_env_var.clone()) {
            (Some(raw), None) => Some(raw),
            (None, Some(name)) => Some(resolve_required_env_api_key(&name)?),
            (None, None) => None,
            (Some(_), Some(_)) => unreachable!("guarded above"),
        };
        return Ok(DiscoveredBackendTarget {
            backend_id: Some(backend_id),
            preset: None,
            provider_kind,
            endpoint,
            api_key: resolved_api_key,
            api_key_env_var,
        });
    }

    let preset = args.backend_preset;
    let api_key = normalize_optional_string(args.api_key.as_deref());
    let explicit_api_key_env_var = normalize_optional_string(args.api_key_env_var.as_deref());
    if api_key.is_some() && explicit_api_key_env_var.is_some() {
        anyhow::bail!("provide either --api-key or --api-key-env-var, not both");
    }
    let endpoint = resolve_backend_endpoint(
        args.endpoint.as_deref(),
        preset,
        BackendResolutionMode::ConfigWrite,
    )?;
    let provider_kind = resolve_backend_provider_kind(args.provider_kind.as_deref(), preset)?;
    let api_key_env_var =
        resolve_backend_api_key_env_var(explicit_api_key_env_var, api_key.is_some(), preset);
    let resolved_api_key = match (api_key, api_key_env_var.clone()) {
        (Some(raw), None) => Some(raw),
        (None, Some(name)) => Some(resolve_required_env_api_key(&name)?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("guarded above"),
    };
    Ok(DiscoveredBackendTarget {
        backend_id: None,
        preset,
        provider_kind,
        endpoint,
        api_key: resolved_api_key,
        api_key_env_var,
    })
}

fn resolve_required_env_api_key(name: &str) -> Result<String> {
    let value = std::env::var(name)
        .with_context(|| format!("required backend API key env var {name} is not set"))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("required backend API key env var {name} is empty");
    }
    Ok(trimmed.to_string())
}

async fn load_backend_row(graphql: &str, backend_id: &str) -> Result<Value> {
    let response = post_graphql(
        graphql,
        &format!(
            r#"{{
                InferenceBackend(
                    filter: {{ backend_id: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    {}
                }}
            }}"#,
            escape_graphql_string(backend_id),
            EXPORT_INFERENCE_BACKEND_FIELDS,
        ),
    )
    .await?;
    response
        .get("data")
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("backend {backend_id} not found"))
}

async fn behavior_set(args: BehaviorUpsertArgs) -> Result<()> {
    let behavior_id = args
        .behavior_id
        .clone()
        .unwrap_or_else(|| default_behavior_id_for_agent(&args.agent_did));
    let system_prompt = match args.system_prompt_file {
        Some(ref path) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("reading system prompt from {}", path.display()))?,
        ),
        None => None,
    };
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let behavior = AgentBehavior {
        behavior_id: behavior_id.clone(),
        agent_did: args.agent_did.clone(),
        display_name: args.display_name.clone(),
        system_prompt,
        backend_id: args.backend_id.clone(),
        model_name: args.model_name.clone(),
        tool_selection_id: args.tool_selection_id.clone(),
        inference_profile_id: args.inference_profile_id.clone(),
        compaction_strategy: args.compaction_strategy.clone(),
        compaction_threshold: args.compaction_threshold,
        enabled: args.enabled,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    let doc_id = write_agent_behavior_document(&access, &behavior).await?;
    let output = json!({
        "doc_id": doc_id,
        "behavior_id": behavior_id,
        "agent_did": args.agent_did,
        "backend_id": args.backend_id,
        "model_name": args.model_name,
        "tool_selection_id": args.tool_selection_id,
        "inference_profile_id": args.inference_profile_id,
        "enabled": args.enabled,
    });
    print_json(&output)?;
    Ok(())
}

async fn tool_selection_set(args: ToolSelectionUpsertArgs) -> Result<()> {
    let file_tools_mode =
        normalize_file_tools_mode(args.enable_file_tools, args.file_tools_mode.as_deref())?;
    let bash_mode = normalize_bash_mode(args.enable_bash, args.bash_mode.as_deref())?;
    let file_tool_root = args
        .file_tool_root
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let selection = ToolSelectionDocument {
        selection_id: args.selection_id.clone(),
        agent_did: args.agent_did.clone(),
        display_name: args.display_name.clone(),
        enable_file_tools: Some(args.enable_file_tools),
        file_tools_mode: Some(file_tools_mode.clone()),
        file_tool_root: file_tool_root.clone(),
        enable_bash: Some(args.enable_bash),
        bash_mode: Some(bash_mode.clone()),
        cli_tool_names: Some(args.cli_tool_names.clone()),
        enable_meta_tools: Some(args.enable_meta_tools),
        delegate_to: Some(args.delegate_to.clone()),
    };
    let doc_id = write_tool_selection_document(&access, &selection).await?;
    let output = json!({
        "doc_id": doc_id,
        "selection_id": args.selection_id,
        "agent_did": args.agent_did,
        "enable_file_tools": args.enable_file_tools,
        "file_tools_mode": file_tools_mode,
        "file_tool_root": file_tool_root,
        "enable_bash": args.enable_bash,
        "bash_mode": bash_mode,
        "cli_tool_names": args.cli_tool_names,
        "enable_meta_tools": args.enable_meta_tools,
        "delegate_to": args.delegate_to,
    });
    print_json(&output)?;
    Ok(())
}

async fn inference_profile_set(args: InferenceProfileUpsertArgs) -> Result<()> {
    let add_fields = vec![
        Some(format!(
            r#"profile_id: "{}""#,
            escape_graphql_string(&args.profile_id)
        )),
        Some(format!(
            r#"display_name: "{}""#,
            escape_graphql_string(args.display_name.as_deref().unwrap_or(""))
        )),
        optional_i64_field("context_window", args.context_window),
        optional_i64_field("max_output_tokens", args.max_output_tokens),
        optional_i64_field("max_turns", args.max_turns),
        optional_f64_field("temperature", args.temperature),
        optional_i64_field("stream_batch_ms", args.stream_batch_ms),
        optional_i64_field("deadline_duration_secs", args.deadline_duration_secs),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let update_fields = vec![
        Some(format!(
            r#"display_name: "{}""#,
            escape_graphql_string(args.display_name.as_deref().unwrap_or(""))
        )),
        optional_i64_field("context_window", args.context_window),
        optional_i64_field("max_output_tokens", args.max_output_tokens),
        optional_i64_field("max_turns", args.max_turns),
        optional_f64_field("temperature", args.temperature),
        optional_i64_field("stream_batch_ms", args.stream_batch_ms),
        optional_i64_field("deadline_duration_secs", args.deadline_duration_secs),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{profile_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        profile_id = escape_graphql_string(&args.profile_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = post_graphql(&args.graphql, &mutation).await?;
    let doc_id = extract_mutation_doc_id(&response, "InferenceProfile")?;
    let output = json!({
        "doc_id": doc_id,
        "profile_id": args.profile_id,
        "display_name": args.display_name,
        "context_window": args.context_window,
        "max_output_tokens": args.max_output_tokens,
        "max_turns": args.max_turns,
        "temperature": args.temperature,
        "stream_batch_ms": args.stream_batch_ms,
        "deadline_duration_secs": args.deadline_duration_secs,
    });
    print_json(&output)?;
    Ok(())
}

async fn scheduled_task_set(args: ScheduledTaskSetArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let access = ConfigAccess::Graphql(graphql.clone());
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let task_id = require_non_empty("task_id", &args.task_id)?;
    let name = require_non_empty("name", &args.name)?;
    if args.interval_secs <= 0 {
        anyhow::bail!("--interval-secs must be greater than zero");
    }

    let prompt = resolve_task_prompt(args.prompt.as_deref(), args.prompt_file.as_deref())?;
    let behavior_id =
        resolve_scheduled_task_behavior_id(&graphql, &agent_did, args.behavior_id.as_deref())
            .await?;
    let next_run_at = normalize_optional_rfc3339(args.next_run_at.as_deref())?;
    let mut add_doc = Map::new();
    add_doc.insert("task_id".to_string(), Value::String(task_id.to_string()));
    add_doc.insert("agent_did".to_string(), Value::String(agent_did.clone()));
    add_doc.insert(
        "behavior_id".to_string(),
        Value::String(behavior_id.clone()),
    );
    add_doc.insert("name".to_string(), Value::String(name.to_string()));
    add_doc.insert("prompt".to_string(), Value::String(prompt.clone()));
    add_doc.insert("interval_secs".to_string(), Value::from(args.interval_secs));
    add_doc.insert("enabled".to_string(), Value::Bool(args.enabled));
    if let Some(next_run_at) = next_run_at.as_ref() {
        add_doc.insert(
            "next_run_at".to_string(),
            Value::String(next_run_at.clone()),
        );
    }

    let update_doc = add_doc.clone();

    let add_doc = Value::Object(add_doc);
    let update_doc = Value::Object(update_doc);

    let doc_id = write_scheduled_task_document(&access, task_id, &add_doc, &update_doc).await?;
    let output = json!({
        "doc_id": doc_id,
        "task_id": task_id,
        "agent_did": agent_did,
        "behavior_id": behavior_id,
        "name": name,
        "interval_secs": args.interval_secs,
        "enabled": args.enabled,
        "next_run_at": next_run_at,
    });
    print_json(&output)?;
    Ok(())
}

async fn request_submit(args: RequestSubmitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let content = resolve_request_content(args.content.as_deref(), args.content_file.as_deref())?;
    let submitted = create_agent_request(
        &graphql,
        &agent_did,
        &content,
        args.session_id.as_deref(),
        args.behavior_id.as_deref(),
        RequestSubmitOptions {
            temperature: args.temperature,
            top_p: args.top_p,
            top_k: args.top_k,
            max_tokens: args.max_tokens,
            metadata: args.metadata.clone(),
        },
    )
    .await?;
    let request_summary = json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "agent_did": submitted.agent_did,
        "behavior_id": submitted.behavior_id,
        "temperature": submitted.temperature,
        "top_p": submitted.top_p,
        "top_k": submitted.top_k,
        "max_tokens": submitted.max_tokens,
        "metadata": submitted.metadata,
    });
    if args.no_wait {
        print_json(&request_summary)?;
        if let Some(path) = args.output_file.as_deref() {
            write_json_output_file(path, &request_summary)?;
        }
        return Ok(());
    }

    let response = wait_for_terminal_response(
        &graphql,
        &submitted.request_id,
        args.timeout_secs,
        args.poll_secs,
    )
    .await
    .with_context(|| format!("waiting for AgentResponse {}", submitted.request_id))?;
    let mut output = request_summary
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("request summary was not a JSON object"))?;
    output.insert("response".to_string(), response);
    let output = serde_json::Value::Object(output);
    print_json(&output)?;
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &output)?;
    }
    Ok(())
}

async fn request_show(args: RequestShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                status
                lifecycle_state
                backend_id
                execution_origin
                failure_reason
                retry_count
                max_retries
                temperature
                top_p
                top_k
                max_tokens
                metadata
                created_at
                claimed_at
                deadline
            }}
        }}"#,
        request_id = escape_graphql_string(&request_id),
    );
    let response = post_graphql(&graphql, &query).await?;
    print_json(&response)?;
    Ok(())
}

async fn response_show(args: ResponseShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let query = response_query(&request_id);
    let response = post_graphql(&graphql, &query).await?;
    print_json(&response)?;
    Ok(())
}

async fn response_wait(args: ResponseWaitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let response =
        wait_for_terminal_response(&graphql, &request_id, args.timeout_secs, args.poll_secs)
            .await?;
    print_json(&response)?;
    Ok(())
}

async fn status(args: StatusArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let output = load_runtime_status_output(args.home.as_deref(), &graphql, &agent_did).await?;
    print_json(&output)?;
    Ok(())
}

async fn p2p_status(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "home": home_dir,
        "graphql": graphql,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn p2p_peers(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let peers = p2p
        .get("p2p_connected_peers")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let count = peers.as_array().map(|rows| rows.len()).unwrap_or(0);
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "home": home_dir,
        "graphql": graphql,
        "p2p": p2p,
        "peers": peers,
        "count": count,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn p2p_connect(args: P2pConnectArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building P2P connect HTTP client")?;
    let api_base = p2p_api_base(&graphql)?;
    http_post_json(
        &client,
        &format!("{api_base}/p2p/connect"),
        &vec![args.peer.clone()],
    )
    .await?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "status": "connect_requested",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn p2p_collections_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let collection_ids: Vec<String> =
        http_get_json(&client, &format!("{api_base}/p2p/collections")).await?;
    let collection_names_by_id = load_collection_name_by_id(&client, &api_base).await;
    let collections = p2p_collection_rows(&collection_ids, &collection_names_by_id);
    let collection_names = p2p_collection_names(&collection_ids, &collection_names_by_id);
    let count = collections.len();
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "ok",
        "home": home_dir,
        "graphql": graphql,
        "collections": collections,
        "collection_ids": collection_ids,
        "collection_names": collection_names,
        "count": count,
    }))?;
    Ok(())
}

async fn p2p_collections_add(args: P2pCollectionsMutateArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections"),
        &collections,
    )
    .await?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "status": "collections_added",
        "home": home_dir,
        "graphql": graphql,
        "collections": collections,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn p2p_collections_remove(args: P2pCollectionsMutateArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    http_delete_json(
        &client,
        &format!("{api_base}/p2p/collections"),
        &collections,
    )
    .await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "collections_removed",
        "home": home_dir,
        "graphql": graphql,
        "collections": collections,
    }))?;
    Ok(())
}

async fn p2p_collections_sync_branchable(args: P2pSyncBranchableArgs) -> Result<()> {
    let collection_id = args.collection_id.trim().to_string();
    if collection_id.is_empty() {
        anyhow::bail!("provide --collection-id");
    }
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let request = P2pSyncBranchableRequest {
        collection_id: collection_id.clone(),
    };
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections/sync-branchable"),
        &request,
    )
    .await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "collection_sync_requested",
        "home": home_dir,
        "graphql": graphql,
        "collection_id": collection_id,
    }))?;
    Ok(())
}

async fn p2p_collections_sync_versions(args: P2pSyncVersionsArgs) -> Result<()> {
    let version_ids = expand_nonempty_values(&args.version_ids, "--version-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let request = P2pSyncVersionsRequest {
        version_ids: version_ids.clone(),
    };
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections/sync-versions"),
        &request,
    )
    .await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "collection_versions_sync_requested",
        "home": home_dir,
        "graphql": graphql,
        "version_ids": version_ids,
    }))?;
    Ok(())
}

async fn p2p_replicators_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let raw_replicators: Vec<P2pReplicatorRow> =
        http_get_json(&client, &format!("{api_base}/p2p/replicators")).await?;
    let collection_names_by_id = load_collection_name_by_id(&client, &api_base).await;
    let replicators = p2p_replicator_rows(raw_replicators, &collection_names_by_id);
    let count = replicators.len();
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "ok",
        "home": home_dir,
        "graphql": graphql,
        "replicators": replicators,
        "count": count,
    }))?;
    Ok(())
}

async fn p2p_replicators_add(args: P2pReplicatorAddArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let request = P2pReplicatorRequest {
        collections: collections.clone(),
        addresses: vec![args.peer.clone()],
    };
    http_post_json(&client, &format!("{api_base}/p2p/replicators"), &request).await?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "status": "replicator_added",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "collections": collections,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn p2p_replicators_remove(args: P2pReplicatorRemoveArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let request = P2pReplicatorDeleteRequest {
        id: args.peer.clone(),
        collections: collections.clone(),
    };
    http_delete_json(&client, &format!("{api_base}/p2p/replicators"), &request).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "replicator_removed",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "collections": collections,
    }))?;
    Ok(())
}

async fn p2p_documents_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let doc_ids: Vec<String> = http_get_json(&client, &format!("{api_base}/p2p/documents")).await?;
    let count = doc_ids.len();
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "ok",
        "home": home_dir,
        "graphql": graphql,
        "doc_ids": doc_ids,
        "count": count,
    }))?;
    Ok(())
}

async fn p2p_documents_add(args: P2pDocumentsMutateArgs) -> Result<()> {
    let doc_ids = expand_nonempty_values(&args.doc_ids, "--doc-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    http_post_json(&client, &format!("{api_base}/p2p/documents"), &doc_ids).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "documents_added",
        "home": home_dir,
        "graphql": graphql,
        "doc_ids": doc_ids,
    }))?;
    Ok(())
}

async fn p2p_documents_remove(args: P2pDocumentsMutateArgs) -> Result<()> {
    let doc_ids = expand_nonempty_values(&args.doc_ids, "--doc-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    http_delete_json(&client, &format!("{api_base}/p2p/documents"), &doc_ids).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "documents_removed",
        "home": home_dir,
        "graphql": graphql,
        "doc_ids": doc_ids,
    }))?;
    Ok(())
}

async fn p2p_documents_sync(args: P2pDocumentsSyncArgs) -> Result<()> {
    let collection = args.collection.trim().to_string();
    if collection.is_empty() {
        anyhow::bail!("provide --collection");
    }
    let doc_ids = expand_nonempty_values(&args.doc_ids, "--doc-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let request = P2pSyncDocumentsRequest {
        collection_name: collection.clone(),
        doc_ids: doc_ids.clone(),
    };
    http_post_json(&client, &format!("{api_base}/p2p/documents/sync"), &request).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "documents_sync_requested",
        "home": home_dir,
        "graphql": graphql,
        "collection": collection,
        "doc_ids": doc_ids,
    }))?;
    Ok(())
}

async fn p2p_diagnose(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let p2p = load_live_http_p2p_status(args.home.as_deref(), &graphql).await;
    let checks = json!({
        "info": p2p_probe_get(&client, &format!("{api_base}/p2p/info")).await,
        "shareable_address": p2p_probe_get(&client, &format!("{api_base}/p2p/shareable-address")).await,
        "peers": p2p_probe_get(&client, &format!("{api_base}/p2p/peers")).await,
        "collections": p2p_probe_get(&client, &format!("{api_base}/p2p/collections")).await,
        "replicators": p2p_probe_get(&client, &format!("{api_base}/p2p/replicators")).await,
        "documents": p2p_probe_get(&client, &format!("{api_base}/p2p/documents")).await,
    });
    let ok = checks.as_object().is_some_and(|map| {
        map.values()
            .all(|value| value.get("ok") == Some(&Value::Bool(true)))
    });
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "status": if ok { "ok" } else { "degraded" },
        "home": home_dir,
        "graphql": graphql,
        "p2p": p2p,
        "checks": {
            "p2p": checks
        }
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn show_runtime(args: RuntimeShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let output = load_runtime_status_output(args.home.as_deref(), &graphql, &agent_did).await?;
    print_json(&output)?;
    Ok(())
}

async fn diagnose(args: DiagnoseArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let init_config = read_init_config(&home_dir)?;
    let runtime_state = read_runtime_state(&home_dir)?;
    let graphql = args
        .graphql
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.graphql.clone()));
    let graphql_reachable = match graphql.as_deref() {
        Some(endpoint) => graphql_endpoint_available(endpoint).await,
        None => false,
    };
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;

    let schema_checks = diagnose_schema_presence(&access).await;
    let bundle_result = build_config_export_bundle(&access, &agent_did).await;
    let config_load_error = bundle_result.as_ref().err().map(ToString::to_string);
    let bundle = bundle_result.unwrap_or_else(|_| ConfigExportBundle {
        format: CONFIG_EXPORT_FORMAT.to_string(),
        agent_did: agent_did.clone(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        access_mode: access.mode().to_string(),
        agent_principal: None,
        agent_behaviors: Vec::new(),
        tool_selections: Vec::new(),
        inference_backends: Vec::new(),
        inference_profiles: Vec::new(),
        tool_service_registries: Vec::new(),
        scheduled_tasks: Vec::new(),
    });
    let runtime_row = match load_runtime_row(&access, &agent_did).await {
        Ok(Some(row)) => row,
        Ok(None) => Value::Null,
        Err(error) => json!({
            "error": error.to_string(),
        }),
    };

    let behavior_ids = bundle
        .agent_behaviors
        .iter()
        .filter_map(|row| {
            row.get("behavior_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let default_behavior_id = bundle
        .agent_principal
        .as_ref()
        .and_then(|row| row.get("default_behavior_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let default_behavior_check = match default_behavior_id.as_deref() {
        Some(behavior_id) if behavior_ids.contains(behavior_id) => json!({
            "ok": true,
            "default_behavior_id": behavior_id,
        }),
        Some(behavior_id) => json!({
            "ok": false,
            "default_behavior_id": behavior_id,
            "error": format!("default behavior {} is not present in AgentBehavior documents", behavior_id),
        }),
        None => json!({
            "ok": false,
            "error": format!("AgentPrincipal {} is missing or has no default_behavior_id", agent_did),
        }),
    };
    let tool_ceiling_check = diagnose_tool_ceiling(init_config.as_ref());
    let backend_reports = diagnose_backends(&bundle).await;
    let matching_runtime_state = runtime_state.as_ref().filter(|state| {
        graphql
            .as_deref()
            .is_some_and(|endpoint| endpoint == state.graphql)
    });
    let p2p_status = match graphql.as_deref().filter(|_| graphql_reachable) {
        Some(endpoint) => load_live_http_p2p_status(args.home.as_deref(), endpoint).await,
        None => persisted_p2p_status(matching_runtime_state),
    };
    let p2p_transport = p2p_status
        .get("p2p_transport")
        .and_then(Value::as_str)
        .unwrap_or(P2pTransportArg::None.as_str());
    let p2p_peer_id = p2p_status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let p2p_connected_peers = p2p_status
        .get("p2p_connected_peers")
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    let p2p_error = p2p_status
        .get("p2p_error")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let p2p_ok = if p2p_transport == P2pTransportArg::None.as_str() {
        true
    } else {
        p2p_peer_id.is_some() && p2p_error.is_none()
    };
    let schemas_ok = schema_checks
        .iter()
        .filter(|check| check.get("required_for_config").and_then(Value::as_bool) == Some(true))
        .all(|check| check.get("ok").and_then(Value::as_bool) == Some(true));
    let backends_ok = backend_reports
        .iter()
        .all(|check| check.get("ok").and_then(Value::as_bool) == Some(true));
    let default_behavior_ok = default_behavior_check
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tool_ceiling_ok = tool_ceiling_check
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let principal_present = bundle.agent_principal.is_some();
    let status = if schemas_ok
        && principal_present
        && default_behavior_ok
        && tool_ceiling_ok
        && backends_ok
        && p2p_ok
        && config_load_error.is_none()
    {
        "ok"
    } else {
        "degraded"
    };

    let mut output = json!({
        "status": status,
        "home": home_dir,
        "agent_did": agent_did,
        "access_mode": access.mode(),
        "graphql": graphql,
        "graphql_reachable": graphql_reachable,
        "runtime": runtime_row,
        "p2p": p2p_status,
        "checks": {
            "schemas": schema_checks,
            "config_documents_loadable": {
                "ok": config_load_error.is_none(),
                "error": config_load_error,
            },
            "agent_principal_present": principal_present,
            "default_behavior": default_behavior_check,
            "tool_ceiling": tool_ceiling_check,
            "backends": backend_reports,
            "p2p": {
                "ok": p2p_ok,
                "transport": p2p_transport,
                "peer_id": p2p_peer_id,
                "connected_peer_count": p2p_connected_peers,
                "error": p2p_error,
            },
        },
        "config_counts": {
            "agent_behaviors": bundle.agent_behaviors.len(),
            "tool_selections": bundle.tool_selections.len(),
            "inference_backends": bundle.inference_backends.len(),
            "inference_profiles": bundle.inference_profiles.len(),
            "tool_service_registries": bundle.tool_service_registries.len(),
            "scheduled_tasks": bundle.scheduled_tasks.len(),
        },
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn config_export(args: ConfigExportArgs) -> Result<()> {
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let bundle = build_config_export_bundle(&access, &agent_did).await?;
    print_json(&serde_json::to_value(bundle)?)?;
    Ok(())
}

async fn config_import(args: ConfigImportArgs) -> Result<()> {
    let bundle = read_config_import_bundle(args.path.as_deref())?;
    validate_config_import_bundle(&bundle)?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    let imported_backends = apply_import_collection(
        &access,
        "InferenceBackend",
        "backend_id",
        &bundle.inference_backends,
        args.override_existing,
    )
    .await?;
    let imported_profiles = apply_import_collection(
        &access,
        "InferenceProfile",
        "profile_id",
        &bundle.inference_profiles,
        args.override_existing,
    )
    .await?;
    let imported_tool_service_registries = apply_import_collection(
        &access,
        "ToolServiceRegistry",
        "service_id",
        &bundle.tool_service_registries,
        args.override_existing,
    )
    .await?;
    let imported_tool_selections = apply_import_collection(
        &access,
        "ToolSelection",
        "selection_id",
        &bundle.tool_selections,
        args.override_existing,
    )
    .await?;
    let imported_behaviors = apply_import_collection(
        &access,
        "AgentBehavior",
        "behavior_id",
        &bundle.agent_behaviors,
        args.override_existing,
    )
    .await?;
    let imported_scheduled_tasks = apply_import_collection(
        &access,
        "ScheduledTask",
        "task_id",
        &bundle.scheduled_tasks,
        args.override_existing,
    )
    .await?;
    let imported_principal = apply_import_collection(
        &access,
        "AgentPrincipal",
        "agent_did",
        &bundle
            .agent_principal
            .clone()
            .into_iter()
            .collect::<Vec<_>>(),
        args.override_existing,
    )
    .await?;

    let output = json!({
        "status": "imported",
        "format": bundle.format,
        "agent_did": bundle.agent_did,
        "access_mode": access.mode(),
        "override": args.override_existing,
        "counts": {
            "agent_principal": imported_principal,
            "agent_behaviors": imported_behaviors,
            "tool_selections": imported_tool_selections,
            "inference_backends": imported_backends,
            "inference_profiles": imported_profiles,
            "tool_service_registries": imported_tool_service_registries,
            "scheduled_tasks": imported_scheduled_tasks,
        },
    });
    print_json(&output)?;
    Ok(())
}

async fn config_validate(args: ConfigValidateArgs) -> Result<()> {
    let report = desired_state::validate_manifest_root(&args.root);
    print_json(&serde_json::to_value(&report)?)?;
    if report.is_ok() {
        Ok(())
    } else {
        anyhow::bail!("desired-state manifest validation failed")
    }
}

fn load_desired_manifest_or_bail(root: &Path) -> Result<desired_state::DesiredStateManifest> {
    let (desired_manifest, validation_report) = desired_state::load_manifest_root(root);
    if !validation_report.is_ok() {
        print_json(&serde_json::to_value(&validation_report)?)?;
        anyhow::bail!("desired-state manifest validation failed")
    }
    desired_manifest.ok_or_else(|| anyhow::anyhow!("validated manifest root produced no manifest"))
}

async fn config_diff(args: ConfigDiffArgs) -> Result<()> {
    let desired_manifest = load_desired_manifest_or_bail(&args.root)?;

    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
    let report = desired_state::diff_manifests(
        &args.root,
        access.mode(),
        &desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
    );
    print_json(&serde_json::to_value(&report)?)?;
    Ok(())
}

async fn config_apply(args: ConfigApplyArgs) -> Result<()> {
    let desired_manifest = load_desired_manifest_or_bail(&args.root)?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let desired_bundle =
        desired_state::export_bundle_from_manifest(&desired_manifest, access.mode())?;

    let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
    let planned = desired_state::diff_manifests(
        &args.root,
        access.mode(),
        &desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
    );

    let applied = apply_desired_state_changes(&access, &desired_bundle, &planned).await?;

    let remaining_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (remaining_principal, remaining_manifest) =
        live_manifest_from_bundle(&desired_manifest, &remaining_bundle)?;
    let remaining = desired_state::diff_manifests(
        &args.root,
        access.mode(),
        &desired_manifest,
        remaining_principal.as_ref(),
        &remaining_manifest,
    );

    let report = ConfigApplyReport {
        status: if config_apply_counts_changed(&applied) {
            "applied"
        } else {
            "noop"
        },
        ok: !diff_has_pending_apply(&remaining.counts),
        exact_match: remaining.ok,
        changed: config_apply_counts_changed(&applied),
        root: args.root.display().to_string(),
        access_mode: access.mode().to_string(),
        agent_did: desired_manifest.agent_principal.agent_did.clone(),
        planned: planned.counts.clone(),
        applied,
        remaining: remaining.counts.clone(),
    };
    print_json(&serde_json::to_value(&report)?)?;
    if report.ok {
        Ok(())
    } else {
        anyhow::bail!("desired-state apply did not converge")
    }
}

async fn load_runtime_status_output(
    home: Option<&Path>,
    graphql: &str,
    agent_did: &str,
) -> Result<Value> {
    let unavailable_behaviors = load_live_unavailable_behaviors(graphql, agent_did).await;
    let query = format!(
        r#"{{
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                agent_did
                process_state
                reconcile_phase
                active_generation
                router_generation
                default_behavior_id
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
                last_reconcile_error
                last_reconcile_completed_at
                updated_at
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let response = post_graphql(graphql, &query).await?;
    let runtime_row = response
        .pointer("/data/AgentRuntime")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or(Value::Null);
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)?;
    let p2p_status = load_live_http_p2p_status(home, graphql).await;
    let mut output = json!({
        "home": home_dir,
        "graphql": graphql,
        "agent_did": agent_did,
        "runtime_state": runtime_state,
        "runtime": runtime_row,
        "p2p": p2p_status,
        "behavior_readiness": if unavailable_behaviors.is_empty() { "ready" } else { "degraded" },
        "unavailable_behaviors": unavailable_behaviors,
    });
    if let Some(map) = output.as_object_mut() {
        for field in [
            "process_state",
            "reconcile_phase",
            "active_generation",
            "router_generation",
            "default_behavior_id",
            "runnable_behavior_count",
            "unavailable_behavior_count",
            "last_reconcile_result",
            "last_reconcile_error",
            "last_reconcile_completed_at",
        ] {
            map.insert(
                field.to_string(),
                runtime_row.get(field).cloned().unwrap_or(Value::Null),
            );
        }
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    Ok(output)
}

async fn load_live_unavailable_behaviors(
    graphql: &str,
    agent_did: &str,
) -> BTreeMap<String, String> {
    let access = ConfigAccess::Graphql(graphql.to_string());
    match build_config_export_bundle(&access, agent_did).await {
        Ok(bundle) => collect_unavailable_behaviors_from_bundle(&bundle),
        Err(_) => BTreeMap::new(),
    }
}

fn collect_unavailable_behaviors_from_bundle(
    bundle: &ConfigExportBundle,
) -> BTreeMap<String, String> {
    let backend_rows = bundle
        .inference_backends
        .iter()
        .filter_map(|row| string_field(row, "backend_id").map(|backend_id| (backend_id, row)))
        .collect::<BTreeMap<_, _>>();
    let tool_selection_rows = bundle
        .tool_selections
        .iter()
        .filter_map(|row| string_field(row, "selection_id").map(|selection_id| (selection_id, row)))
        .collect::<BTreeMap<_, _>>();
    let inference_profile_rows = bundle
        .inference_profiles
        .iter()
        .filter_map(|row| string_field(row, "profile_id").map(|profile_id| (profile_id, row)))
        .collect::<BTreeMap<_, _>>();

    let mut unavailable = BTreeMap::new();
    for behavior in &bundle.agent_behaviors {
        let Some(behavior_id) = string_field(behavior, "behavior_id") else {
            continue;
        };
        if !bool_field(behavior, "enabled", true) {
            unavailable.insert(
                behavior_id.clone(),
                format!("behavior {behavior_id} is disabled"),
            );
            continue;
        }

        let Some(backend_id) = string_field(behavior, "backend_id") else {
            unavailable.insert(
                behavior_id.clone(),
                format!("behavior {behavior_id} has no backend binding"),
            );
            continue;
        };
        let Some(backend) = backend_rows.get(&backend_id) else {
            unavailable.insert(
                behavior_id.clone(),
                format!("behavior {behavior_id} references missing backend {backend_id}"),
            );
            continue;
        };

        let probe_status =
            string_field(backend, "probe_status").unwrap_or_else(|| "unknown".to_string());
        let backend_enabled = bool_field(backend, "enabled", true);
        if !backend_enabled || probe_status != "healthy" {
            unavailable.insert(
                behavior_id.clone(),
                format!(
                    "behavior {behavior_id} backend {backend_id} is unavailable (enabled={backend_enabled} probe_status={probe_status})"
                ),
            );
            continue;
        }

        if let Some(profile_id) = string_field(behavior, "inference_profile_id") {
            if !inference_profile_rows.contains_key(&profile_id) {
                unavailable.insert(
                    behavior_id.clone(),
                    format!(
                        "behavior {behavior_id} references missing inference profile {profile_id}"
                    ),
                );
                continue;
            }
        }

        let _tool_selection = match string_field(behavior, "tool_selection_id") {
            Some(selection_id) => match tool_selection_rows.get(&selection_id) {
                Some(row) => Some(*row),
                None => {
                    unavailable.insert(
                        behavior_id.clone(),
                        format!(
                            "behavior {behavior_id} references missing tool selection {selection_id}"
                        ),
                    );
                    continue;
                }
            },
            None => None,
        };
    }

    unavailable
}

fn string_field(row: &Value, field: &str) -> Option<String> {
    normalize_optional_string(row.get(field).and_then(Value::as_str))
}

fn bool_field(row: &Value, field: &str, default: bool) -> bool {
    row.get(field).and_then(Value::as_bool).unwrap_or(default)
}

fn persisted_p2p_status(runtime_state: Option<&StoredRuntimeState>) -> Value {
    match runtime_state {
        Some(runtime_state) => json!({
            "enabled": runtime_state.p2p_transport != P2pTransportArg::None.as_str(),
            "p2p_transport": runtime_state.p2p_transport,
            "p2p_peer_id": runtime_state.p2p_peer_id,
            "p2p_listen_addresses": runtime_state.p2p_listen_addresses,
            "p2p_shareable_address": Value::Null,
            "p2p_connected_peers": [],
            "p2p_error": Value::Null,
        }),
        None => json!({
            "enabled": false,
            "p2p_transport": P2pTransportArg::None.as_str(),
            "p2p_peer_id": Value::Null,
            "p2p_listen_addresses": [],
            "p2p_shareable_address": Value::Null,
            "p2p_connected_peers": [],
            "p2p_error": Value::Null,
        }),
    }
}

fn expand_p2p_collection_args(
    explicit_collections: &[String],
    profiles: &[P2pCollectionProfileArg],
) -> Result<Vec<String>> {
    let mut collections = explicit_collections
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();

    for profile in profiles {
        for collection in p2p_collection_profile_names(*profile) {
            collections.insert(collection.to_string());
        }
    }

    if collections.is_empty() {
        anyhow::bail!("provide at least one --collection or --profile");
    }

    Ok(collections.into_iter().collect())
}

fn p2p_collection_profile_names(profile: P2pCollectionProfileArg) -> Vec<&'static str> {
    match profile {
        P2pCollectionProfileArg::Runtime => SCHEMA_COLLECTION_CHECKS
            .iter()
            .map(|(collection, _)| *collection)
            .collect(),
        P2pCollectionProfileArg::Agent => P2P_AGENT_COLLECTIONS.to_vec(),
        P2pCollectionProfileArg::DesktopConfig => P2P_DESKTOP_CONFIG_COLLECTIONS.to_vec(),
        P2pCollectionProfileArg::ChatRequests => P2P_CHAT_REQUEST_COLLECTIONS.to_vec(),
        P2pCollectionProfileArg::ToolServices => P2P_TOOL_SERVICE_COLLECTIONS.to_vec(),
    }
}

fn expand_nonempty_values(values: &[String], flag_name: &str) -> Result<Vec<String>> {
    let values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();

    if values.is_empty() {
        anyhow::bail!("provide at least one {flag_name}");
    }

    Ok(values.into_iter().collect())
}

async fn load_collection_name_by_id(
    client: &reqwest::Client,
    api_base: &str,
) -> BTreeMap<String, String> {
    let Ok(collections) =
        http_get_json::<Vec<Value>>(client, &format!("{api_base}/collections/versions")).await
    else {
        return BTreeMap::new();
    };

    collections
        .into_iter()
        .filter_map(|row| {
            let id = collection_version_string_field(&row, &["CollectionID", "collection_id"])?;
            let name = collection_version_string_field(&row, &["Name", "name"])?;
            Some((id, name))
        })
        .collect()
}

fn collection_version_string_field(row: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        row.get(*name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn p2p_collection_rows(
    collection_ids: &[String],
    collection_names_by_id: &BTreeMap<String, String>,
) -> Vec<P2pCollectionSubscriptionRow> {
    collection_ids
        .iter()
        .map(|id| P2pCollectionSubscriptionRow {
            id: id.clone(),
            name: collection_names_by_id.get(id).cloned(),
        })
        .collect()
}

fn p2p_collection_names(
    collection_ids: &[String],
    collection_names_by_id: &BTreeMap<String, String>,
) -> Vec<String> {
    collection_ids
        .iter()
        .filter_map(|id| collection_names_by_id.get(id).cloned())
        .collect()
}

fn p2p_replicator_rows(
    rows: Vec<P2pReplicatorRow>,
    collection_names_by_id: &BTreeMap<String, String>,
) -> Vec<P2pReplicatorOutputRow> {
    rows.into_iter()
        .map(|row| {
            let collection_names =
                p2p_collection_names(&row.collection_ids, collection_names_by_id);
            P2pReplicatorOutputRow {
                id: row.id,
                addresses: row.addresses,
                collection_ids: row.collection_ids,
                collection_names,
            }
        })
        .collect()
}

async fn load_live_http_p2p_status(home: Option<&Path>, graphql: &str) -> Value {
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)
        .ok()
        .flatten()
        .filter(|state| state.graphql == graphql);
    match fetch_live_http_p2p_status(home, graphql).await {
        Ok(status) => status,
        Err(error) => {
            let mut status = persisted_p2p_status(runtime_state.as_ref());
            if let Some(map) = status.as_object_mut() {
                map.insert("p2p_error".to_string(), Value::String(error.to_string()));
            }
            status
        }
    }
}

async fn fetch_live_http_p2p_status(home: Option<&Path>, graphql: &str) -> Result<Value> {
    let home_dir = resolve_home_dir(home);
    let runtime_state = read_runtime_state(&home_dir)?.filter(|state| state.graphql == graphql);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building P2P status HTTP client")?;
    let api_base = p2p_api_base(graphql)?;
    let identity =
        http_get_json::<NodeIdentityResponse>(&client, &format!("{api_base}/node/identity"))
            .await
            .ok();
    let transport = runtime_state
        .as_ref()
        .map(|state| state.p2p_transport.as_str())
        .filter(|transport| !transport.is_empty())
        .unwrap_or(P2pTransportArg::None.as_str());
    let listen_addresses: Vec<String> =
        http_get_json(&client, &format!("{api_base}/p2p/info")).await?;
    let shareable_address: P2pShareableAddressResponse =
        http_get_json(&client, &format!("{api_base}/p2p/shareable-address")).await?;
    let shareable_address = normalize_optional_string(shareable_address.address.as_deref())
        .context("runtime reported an empty shareable P2P address")?;
    let peer_id = resolve_p2p_peer_id(
        identity
            .as_ref()
            .and_then(|identity| identity.peer_id.as_deref()),
        Some(&shareable_address),
        &listen_addresses,
        runtime_state
            .as_ref()
            .and_then(|state| state.p2p_peer_id.as_deref()),
    )
    .context("runtime reported a shareable P2P address but no usable peer id")?;
    let peer_rows: Vec<P2pPeerRow> =
        http_get_json(&client, &format!("{api_base}/p2p/peers")).await?;
    let connected_peers = peer_rows.into_iter().map(|row| row.id).collect::<Vec<_>>();
    Ok(json!({
        "enabled": true,
        "p2p_transport": if transport == P2pTransportArg::None.as_str() {
            P2pTransportArg::Iroh.as_str()
        } else {
            transport
        },
        "p2p_peer_id": peer_id,
        "p2p_listen_addresses": listen_addresses,
        "p2p_shareable_address": shareable_address,
        "p2p_connected_peers": connected_peers,
        "p2p_error": Value::Null,
    }))
}

fn p2p_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building P2P HTTP client")
}

async fn p2p_probe_get(client: &reqwest::Client, url: &str) -> Value {
    match http_get_json::<Value>(client, url).await {
        Ok(value) => json!({
            "ok": true,
            "value": value,
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
        }),
    }
}

fn p2p_api_base(graphql: &str) -> Result<String> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow::anyhow!("expected GraphQL endpoint ending in /graphql, got {graphql}")
        })
}

async fn http_get_json<T: DeserializeOwned>(client: &reqwest::Client, url: &str) -> Result<T> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("sending GET request to {url}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading GET response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "GET {url} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body).with_context(|| format!("decoding JSON response from {url}"))
}

async fn http_post_json<B: Serialize>(client: &reqwest::Client, url: &str, body: &B) -> Result<()> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("sending POST request to {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading POST response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "POST {url} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}

async fn http_delete_json<B: Serialize>(
    client: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<()> {
    let response = client
        .delete(url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("sending DELETE request to {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading DELETE response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "DELETE {url} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}

fn flatten_p2p_fields(map: &mut serde_json::Map<String, Value>, p2p: &Value) {
    map.insert(
        "p2p_enabled".to_string(),
        p2p.get("enabled").cloned().unwrap_or(Value::Bool(false)),
    );
    for field in [
        "p2p_transport",
        "p2p_peer_id",
        "p2p_listen_addresses",
        "p2p_shareable_address",
        "p2p_connected_peers",
        "p2p_error",
    ] {
        map.insert(
            field.to_string(),
            p2p.get(field).cloned().unwrap_or(Value::Null),
        );
    }
}

async fn resolve_config_access(
    home: Option<&Path>,
    explicit_graphql: Option<&str>,
    ensure_local_schemas: bool,
) -> Result<(ConfigAccess, PathBuf)> {
    let home_dir = resolve_home_dir(home);
    if let Some(graphql) = explicit_graphql
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok((ConfigAccess::Graphql(graphql.to_string()), home_dir));
    }
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        if graphql_endpoint_available(&runtime_state.graphql).await {
            return Ok((ConfigAccess::Graphql(runtime_state.graphql), home_dir));
        }
    }

    let data_dir = default_data_dir(&home_dir);
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .build()
        .await
        .with_context(|| format!("building embedded defra node from {}", data_dir.display()))?;
    if ensure_local_schemas {
        ensure_runtime_schemas(&node).await?;
    }
    Ok((ConfigAccess::Local(node), home_dir))
}

async fn graphql_endpoint_available(graphql: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client
        .post(graphql)
        .json(&json!({ "query": "{ __typename }" }))
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

async fn load_runtime_row(access: &ConfigAccess, agent_did: &str) -> Result<Option<Value>> {
    let query = format!(
        r#"{{
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                agent_did
                process_state
                reconcile_phase
                active_generation
                router_generation
                default_behavior_id
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
                last_reconcile_error
                last_reconcile_completed_at
                updated_at
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    Ok(graphql_rows(access, "AgentRuntime", &query)
        .await?
        .into_iter()
        .next())
}

async fn graphql_rows(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let response = access.execute(query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

async fn graphql_rows_or_empty_if_collection_missing(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    match graphql_rows(access, collection_name, query).await {
        Ok(rows) => Ok(rows),
        Err(error) if is_collection_missing_error(collection_name, &error) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn is_collection_missing_error(collection_name: &str, error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains(collection_name)
        && (message.contains("collection not found") || message.contains("Cannot query field"))
}

async fn build_config_export_bundle(
    access: &ConfigAccess,
    agent_did: &str,
) -> Result<ConfigExportBundle> {
    let principal_rows = graphql_rows(
        access,
        "AgentPrincipal",
        &format!(
            r#"{{
                AgentPrincipal(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    limit: 1
                ) {{
                    {fields}
                }}
            }}"#,
            agent_did = escape_graphql_string(agent_did),
            fields = EXPORT_AGENT_PRINCIPAL_FIELDS,
        ),
    )
    .await?;
    let mut behavior_rows = graphql_rows(
        access,
        "AgentBehavior",
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    order: {{ created_at: ASC }}
                ) {{
                    {fields}
                }}
            }}"#,
            agent_did = escape_graphql_string(agent_did),
            fields = EXPORT_AGENT_BEHAVIOR_FIELDS,
        ),
    )
    .await?;
    sort_document_rows(&mut behavior_rows, "behavior_id");

    let tool_selection_ids = collect_string_field_values(&behavior_rows, "tool_selection_id");
    let backend_ids = collect_string_field_values(&behavior_rows, "backend_id");
    let profile_ids = collect_string_field_values(&behavior_rows, "inference_profile_id");

    let mut tool_selection_rows = if tool_selection_ids.is_empty() {
        Vec::new()
    } else {
        graphql_rows(
            access,
            "ToolSelection",
            &format!(
                r#"{{
                    ToolSelection(
                        filter: {{ selection_id: {{ _in: {} }} }}
                    ) {{
                        {fields}
                    }}
                }}"#,
                graphql_string_list_literal(&tool_selection_ids),
                fields = EXPORT_TOOL_SELECTION_FIELDS,
            ),
        )
        .await?
    };
    sort_document_rows(&mut tool_selection_rows, "selection_id");

    let mut backend_rows = if backend_ids.is_empty() {
        Vec::new()
    } else {
        graphql_rows(
            access,
            "InferenceBackend",
            &format!(
                r#"{{
                    InferenceBackend(
                        filter: {{ backend_id: {{ _in: {} }} }}
                    ) {{
                        {fields}
                    }}
                }}"#,
                graphql_string_list_literal(&backend_ids),
                fields = EXPORT_INFERENCE_BACKEND_FIELDS,
            ),
        )
        .await?
    };
    sort_document_rows(&mut backend_rows, "backend_id");

    let mut profile_rows = if profile_ids.is_empty() {
        Vec::new()
    } else {
        graphql_rows(
            access,
            "InferenceProfile",
            &format!(
                r#"{{
                    InferenceProfile(
                        filter: {{ profile_id: {{ _in: {} }} }}
                    ) {{
                        {fields}
                    }}
                }}"#,
                graphql_string_list_literal(&profile_ids),
                fields = EXPORT_INFERENCE_PROFILE_FIELDS,
            ),
        )
        .await?
    };
    sort_document_rows(&mut profile_rows, "profile_id");
    let mut tool_service_registry_rows = graphql_rows_or_empty_if_collection_missing(
        access,
        "ToolServiceRegistry",
        &format!(
            r#"{{
                ToolServiceRegistry {{
                    {fields}
                }}
            }}"#,
            fields = EXPORT_TOOL_SERVICE_REGISTRY_FIELDS,
        ),
    )
    .await?;
    sort_document_rows(&mut tool_service_registry_rows, "service_id");
    normalize_tool_service_registry_export_rows(&mut tool_service_registry_rows)?;
    let mut scheduled_task_rows = graphql_rows_or_empty_if_collection_missing(
        access,
        "ScheduledTask",
        &format!(
            r#"{{
                ScheduledTask(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}
                ) {{
                    {fields}
                }}
            }}"#,
            agent_did = escape_graphql_string(agent_did),
            fields = EXPORT_SCHEDULED_TASK_FIELDS,
        ),
    )
    .await?;
    sort_document_rows(&mut scheduled_task_rows, "task_id");

    Ok(ConfigExportBundle {
        format: CONFIG_EXPORT_FORMAT.to_string(),
        agent_did: agent_did.to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        access_mode: access.mode().to_string(),
        agent_principal: principal_rows.into_iter().next(),
        agent_behaviors: behavior_rows,
        tool_selections: tool_selection_rows,
        inference_backends: backend_rows,
        inference_profiles: profile_rows,
        tool_service_registries: tool_service_registry_rows,
        scheduled_tasks: scheduled_task_rows,
    })
}

async fn build_desired_state_live_bundle(
    access: &ConfigAccess,
    desired_manifest: &desired_state::DesiredStateManifest,
) -> Result<ConfigExportBundle> {
    let agent_did = desired_manifest.agent_principal.agent_did.as_str();
    let principal_rows = graphql_rows(
        access,
        "AgentPrincipal",
        &format!(
            r#"{{
                AgentPrincipal(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    limit: 1
                ) {{
                    {fields}
                }}
            }}"#,
            agent_did = escape_graphql_string(agent_did),
            fields = EXPORT_AGENT_PRINCIPAL_FIELDS,
        ),
    )
    .await?;
    let mut behavior_rows = graphql_rows(
        access,
        "AgentBehavior",
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    order: {{ created_at: ASC }}
                ) {{
                    {fields}
                }}
            }}"#,
            agent_did = escape_graphql_string(agent_did),
            fields = EXPORT_AGENT_BEHAVIOR_FIELDS,
        ),
    )
    .await?;
    sort_document_rows(&mut behavior_rows, "behavior_id");

    let tool_selection_ids = collect_string_field_values(&behavior_rows, "tool_selection_id")
        .into_iter()
        .chain(
            desired_manifest
                .tool_selections
                .iter()
                .map(|value| value.selection_id.clone()),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let backend_ids = collect_string_field_values(&behavior_rows, "backend_id")
        .into_iter()
        .chain(
            desired_manifest
                .inference_backends
                .iter()
                .map(|value| value.backend_id.clone()),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let profile_ids = collect_string_field_values(&behavior_rows, "inference_profile_id")
        .into_iter()
        .chain(
            desired_manifest
                .inference_profiles
                .iter()
                .map(|value| value.profile_id.clone()),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let mut tool_selection_rows = if tool_selection_ids.is_empty() {
        Vec::new()
    } else {
        graphql_rows(
            access,
            "ToolSelection",
            &format!(
                r#"{{
                    ToolSelection(
                        filter: {{ selection_id: {{ _in: {} }} }}
                    ) {{
                        {fields}
                    }}
                }}"#,
                graphql_string_list_literal(&tool_selection_ids),
                fields = EXPORT_TOOL_SELECTION_FIELDS,
            ),
        )
        .await?
    };
    sort_document_rows(&mut tool_selection_rows, "selection_id");

    let mut backend_rows = if backend_ids.is_empty() {
        Vec::new()
    } else {
        graphql_rows(
            access,
            "InferenceBackend",
            &format!(
                r#"{{
                    InferenceBackend(
                        filter: {{ backend_id: {{ _in: {} }} }}
                    ) {{
                        {fields}
                    }}
                }}"#,
                graphql_string_list_literal(&backend_ids),
                fields = EXPORT_INFERENCE_BACKEND_FIELDS,
            ),
        )
        .await?
    };
    sort_document_rows(&mut backend_rows, "backend_id");

    let mut profile_rows = if profile_ids.is_empty() {
        Vec::new()
    } else {
        graphql_rows(
            access,
            "InferenceProfile",
            &format!(
                r#"{{
                    InferenceProfile(
                        filter: {{ profile_id: {{ _in: {} }} }}
                    ) {{
                        {fields}
                    }}
                }}"#,
                graphql_string_list_literal(&profile_ids),
                fields = EXPORT_INFERENCE_PROFILE_FIELDS,
            ),
        )
        .await?
    };
    sort_document_rows(&mut profile_rows, "profile_id");
    let tool_service_ids = desired_manifest
        .tool_service_registries
        .iter()
        .map(|value| value.service_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut tool_service_registry_rows = if tool_service_ids.is_empty() {
        Vec::new()
    } else {
        graphql_rows_or_empty_if_collection_missing(
            access,
            "ToolServiceRegistry",
            &format!(
                r#"{{
                    ToolServiceRegistry(
                        filter: {{ service_id: {{ _in: {} }} }}
                    ) {{
                        {fields}
                    }}
                }}"#,
                graphql_string_list_literal(&tool_service_ids),
                fields = EXPORT_TOOL_SERVICE_REGISTRY_FIELDS,
            ),
        )
        .await?
    };
    sort_document_rows(&mut tool_service_registry_rows, "service_id");
    let mut scheduled_task_rows = graphql_rows_or_empty_if_collection_missing(
        access,
        "ScheduledTask",
        &format!(
            r#"{{
                ScheduledTask(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}
                ) {{
                    {fields}
                }}
            }}"#,
            agent_did = escape_graphql_string(agent_did),
            fields = EXPORT_SCHEDULED_TASK_FIELDS,
        ),
    )
    .await?;
    sort_document_rows(&mut scheduled_task_rows, "task_id");

    Ok(ConfigExportBundle {
        format: CONFIG_EXPORT_FORMAT.to_string(),
        agent_did: agent_did.to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        access_mode: access.mode().to_string(),
        agent_principal: principal_rows.into_iter().next(),
        agent_behaviors: behavior_rows,
        tool_selections: tool_selection_rows,
        inference_backends: backend_rows,
        inference_profiles: profile_rows,
        tool_service_registries: tool_service_registry_rows,
        scheduled_tasks: scheduled_task_rows,
    })
}

fn live_manifest_from_bundle(
    desired_manifest: &desired_state::DesiredStateManifest,
    live_bundle: &ConfigExportBundle,
) -> Result<(
    Option<desired_state::DesiredAgentPrincipal>,
    desired_state::DesiredStateManifest,
)> {
    if live_bundle.agent_principal.is_some() {
        let live_manifest = desired_state::manifest_from_export_bundle(live_bundle)?;
        Ok((Some(live_manifest.agent_principal.clone()), live_manifest))
    } else {
        Ok((
            None,
            desired_state::DesiredStateManifest {
                agent_principal: desired_manifest.agent_principal.clone(),
                agent_behaviors: Vec::new(),
                tool_selections: Vec::new(),
                inference_backends: Vec::new(),
                inference_profiles: Vec::new(),
                tool_service_registries: Vec::new(),
                scheduled_tasks: Vec::new(),
            },
        ))
    }
}

fn sort_document_rows(rows: &mut [Value], key: &str) {
    rows.sort_by(|left, right| {
        let left_key = left.get(key).and_then(Value::as_str).unwrap_or_default();
        let right_key = right.get(key).and_then(Value::as_str).unwrap_or_default();
        left_key.cmp(right_key)
    });
}

fn normalize_tool_service_registry_export_rows(rows: &mut [Value]) -> Result<()> {
    for row in rows {
        let object = row
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("ToolServiceRegistry export row must be an object"))?;
        desired_state::normalize_tool_service_registry_storage_fields(object)?;
    }
    Ok(())
}

fn collect_string_field_values(rows: &[Value], field: &str) -> Vec<String> {
    let mut values = rows
        .iter()
        .filter_map(|row| row.get(field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn graphql_string_list_literal(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn read_config_import_bundle(path: Option<&Path>) -> Result<ConfigExportBundle> {
    let contents = match path {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("reading config import from {}", path.display()))?,
        None => {
            let mut contents = String::new();
            io::stdin()
                .read_to_string(&mut contents)
                .context("reading config import from stdin")?;
            contents
        }
    };
    let mut bundle: ConfigExportBundle =
        serde_json::from_str(&contents).context("decoding config import JSON")?;
    migrate_config_import_bundle(&mut bundle);
    Ok(bundle)
}

fn validate_config_import_bundle(bundle: &ConfigExportBundle) -> Result<()> {
    if !matches!(
        bundle.format.as_str(),
        CONFIG_EXPORT_FORMAT | CONFIG_EXPORT_FORMAT_V1
    ) {
        anyhow::bail!(
            "unsupported config import format {}; expected {}",
            bundle.format,
            CONFIG_EXPORT_FORMAT
        );
    }
    if bundle.agent_did.trim().is_empty() {
        anyhow::bail!("config import is missing agent_did");
    }
    Ok(())
}

fn migrate_config_import_bundle(bundle: &mut ConfigExportBundle) {
    for backend in &mut bundle.inference_backends {
        if let Some(object) = backend.as_object_mut() {
            desired_state::strip_deprecated_inference_backend_fields(object);
        }
    }
    if bundle.format == CONFIG_EXPORT_FORMAT_V1 {
        bundle.format = CONFIG_EXPORT_FORMAT.to_string();
    }
}

async fn apply_import_collection(
    access: &ConfigAccess,
    collection_name: &str,
    unique_field: &str,
    docs: &[Value],
    override_existing: bool,
) -> Result<usize> {
    for doc in docs {
        let unique_value = doc
            .get(unique_field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} import document is missing {}: {}",
                    collection_name,
                    unique_field,
                    doc
                )
            })?;
        let add_doc = sanitize_import_document(collection_name, doc, false)?;
        if override_existing && collection_name == "ScheduledTask" {
            let update_doc = sanitize_import_document(collection_name, doc, true)?;
            let doc_id = write_scheduled_task_document(access, unique_value, &add_doc, &update_doc)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "importing {collection_name} {} failed: {error}",
                        unique_value
                    )
                })?;
            if doc_id.trim().is_empty() {
                anyhow::bail!(
                    "importing {collection_name} {} returned an empty _docID",
                    unique_value
                );
            }
            continue;
        }

        let add_literal = graphql_input_literal(&add_doc)?;
        let mutation = if override_existing {
            let update_doc = sanitize_import_document(collection_name, doc, true)?;
            let update_literal = graphql_input_literal(&update_doc)?;
            format!(
                r#"mutation {{
                    upsert_{collection_name}(
                        filter: {{ {unique_field}: {{ _eq: "{unique_value}" }} }},
                        add: {add_literal},
                        update: {update_literal}
                    ) {{ _docID }}
                }}"#,
                collection_name = collection_name,
                unique_field = unique_field,
                unique_value = escape_graphql_string(unique_value),
                add_literal = add_literal,
                update_literal = update_literal,
            )
        } else {
            format!(
                r#"mutation {{
                    create_{collection_name}(input: {add_literal}) {{ _docID }}
                }}"#,
                collection_name = collection_name,
                add_literal = add_literal,
            )
        };
        let response = access.execute(&mutation).await.map_err(|error| {
            if override_existing {
                anyhow::anyhow!(
                    "importing {collection_name} {} failed: {error}",
                    unique_value
                )
            } else {
                anyhow::anyhow!(
                    "importing {collection_name} {} failed: {error}\nNext:\n  1. If the document already exists, rerun with `defra-agent config import --override`\n  2. Or remove the existing document and retry",
                    unique_value
                )
            }
        })?;
        let _ = extract_mutation_doc_id(&response, collection_name)?;
    }

    Ok(docs.len())
}

fn sanitize_import_document(collection_name: &str, doc: &Value, for_update: bool) -> Result<Value> {
    let mut object = match collection_name {
        "InferenceBackend" | "ScheduledTask" | "ToolServiceRegistry" => {
            doc.as_object().cloned().ok_or_else(|| {
                anyhow::anyhow!("{collection_name} import document must be an object")
            })?
        }
        _ => return Ok(doc.clone()),
    };

    match collection_name {
        "InferenceBackend" => {
            desired_state::strip_deprecated_inference_backend_fields(&mut object);
            object.remove("last_probe");
            if for_update {
                object.insert("last_probe".to_string(), Value::Null);
            }
        }
        "ScheduledTask" => {
            for field in [
                "next_run_at",
                "last_run_at",
                "last_status",
                "last_error",
                "run_count",
                "created_at",
                "updated_at",
            ] {
                object.remove(field);
            }
            if for_update {
                object.insert("next_run_at".to_string(), Value::Null);
                object.insert("last_run_at".to_string(), Value::Null);
                object.insert("created_at".to_string(), Value::Null);
                object.insert("updated_at".to_string(), Value::Null);
            }
        }
        "ToolServiceRegistry" => {
            for field in ["tools", "version", "updated_at"] {
                object.remove(field);
            }
            desired_state::normalize_tool_service_registry_storage_fields(&mut object)?;
            if for_update {
                object.insert("updated_at".to_string(), Value::Null);
            }
            match object.get("status") {
                Some(Value::String(s)) if !s.is_empty() => {}
                _ => {
                    object.insert("status".to_string(), Value::String("online".to_string()));
                }
            }
        }
        _ => unreachable!(),
    }

    Ok(Value::Object(object))
}

fn diff_has_pending_apply(counts: &desired_state::DesiredStateDiffCollectionsCounts) -> bool {
    [
        &counts.agent_principal,
        &counts.agent_behaviors,
        &counts.tool_selections,
        &counts.inference_backends,
        &counts.inference_profiles,
        &counts.tool_service_registries,
        &counts.scheduled_tasks,
    ]
    .iter()
    .any(|count| count.create > 0 || count.update > 0)
}

fn config_apply_counts_changed(counts: &ConfigApplyCounts) -> bool {
    counts.agent_principal > 0
        || counts.agent_behaviors > 0
        || counts.tool_selections > 0
        || counts.inference_backends > 0
        || counts.inference_profiles > 0
        || counts.tool_service_registries > 0
        || counts.scheduled_tasks > 0
}

fn select_apply_collection_docs(
    docs: &[Value],
    unique_field: &str,
    collection_name: &str,
    diff: &desired_state::DesiredStateCollectionDiff,
) -> Result<Vec<Value>> {
    let requested_ids = diff
        .create
        .iter()
        .chain(diff.update.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    if requested_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut selected = docs
        .iter()
        .filter(|doc| {
            doc.get(unique_field)
                .and_then(Value::as_str)
                .is_some_and(|value| requested_ids.contains(value))
        })
        .cloned()
        .collect::<Vec<_>>();
    sort_document_rows(&mut selected, unique_field);

    let found_ids = selected
        .iter()
        .filter_map(|doc| doc.get(unique_field).and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    let missing_ids = requested_ids
        .difference(&found_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_ids.is_empty() {
        anyhow::bail!(
            "desired-state apply missing {collection_name} documents for ids: {}",
            missing_ids.join(", ")
        );
    }

    Ok(selected)
}

fn select_apply_principal_docs(
    doc: Option<&Value>,
    diff: &desired_state::DesiredStateCollectionDiff,
) -> Result<Vec<Value>> {
    if diff.create.is_empty() && diff.update.is_empty() {
        return Ok(Vec::new());
    }
    let doc =
        doc.ok_or_else(|| anyhow::anyhow!("desired-state apply is missing AgentPrincipal"))?;
    Ok(vec![doc.clone()])
}

async fn apply_desired_state_changes(
    access: &ConfigAccess,
    desired_bundle: &ConfigExportBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    let backend_docs = select_apply_collection_docs(
        &desired_bundle.inference_backends,
        "backend_id",
        "InferenceBackend",
        &planned.collections.inference_backends,
    )?;
    let profile_docs = select_apply_collection_docs(
        &desired_bundle.inference_profiles,
        "profile_id",
        "InferenceProfile",
        &planned.collections.inference_profiles,
    )?;
    let tool_selection_docs = select_apply_collection_docs(
        &desired_bundle.tool_selections,
        "selection_id",
        "ToolSelection",
        &planned.collections.tool_selections,
    )?;
    let tool_service_registry_docs = select_apply_collection_docs(
        &desired_bundle.tool_service_registries,
        "service_id",
        "ToolServiceRegistry",
        &planned.collections.tool_service_registries,
    )?;
    let behavior_docs = select_apply_collection_docs(
        &desired_bundle.agent_behaviors,
        "behavior_id",
        "AgentBehavior",
        &planned.collections.agent_behaviors,
    )?;
    let scheduled_task_docs = select_apply_collection_docs(
        &desired_bundle.scheduled_tasks,
        "task_id",
        "ScheduledTask",
        &planned.collections.scheduled_tasks,
    )?;
    let principal_docs = select_apply_principal_docs(
        desired_bundle.agent_principal.as_ref(),
        &planned.collections.agent_principal,
    )?;

    Ok(ConfigApplyCounts {
        inference_backends: apply_import_collection(
            access,
            "InferenceBackend",
            "backend_id",
            &backend_docs,
            true,
        )
        .await?,
        inference_profiles: apply_import_collection(
            access,
            "InferenceProfile",
            "profile_id",
            &profile_docs,
            true,
        )
        .await?,
        tool_service_registries: apply_import_collection(
            access,
            "ToolServiceRegistry",
            "service_id",
            &tool_service_registry_docs,
            true,
        )
        .await?,
        tool_selections: apply_import_collection(
            access,
            "ToolSelection",
            "selection_id",
            &tool_selection_docs,
            true,
        )
        .await?,
        agent_behaviors: apply_import_collection(
            access,
            "AgentBehavior",
            "behavior_id",
            &behavior_docs,
            true,
        )
        .await?,
        scheduled_tasks: apply_import_collection(
            access,
            "ScheduledTask",
            "task_id",
            &scheduled_task_docs,
            true,
        )
        .await?,
        agent_principal: apply_import_collection(
            access,
            "AgentPrincipal",
            "agent_did",
            &principal_docs,
            true,
        )
        .await?,
    })
}

fn graphql_input_literal(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(graphql_string_literal(value)),
        Value::Array(values) => {
            let rendered = values
                .iter()
                .map(graphql_input_literal)
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", rendered.join(", ")))
        }
        Value::Object(map) => {
            let rendered = map
                .iter()
                .map(|(key, value)| Ok(format!("{key}: {}", graphql_input_literal(value)?)))
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("{{ {} }}", rendered.join(", ")))
        }
    }
}

async fn diagnose_schema_presence(access: &ConfigAccess) -> Vec<Value> {
    let mut results = Vec::new();
    for (collection, field) in SCHEMA_COLLECTION_CHECKS {
        let required_for_config = CONFIG_SCHEMA_COLLECTIONS.contains(collection);
        let query = format!(
            r#"{{ {collection}(limit: 1) {{ {field} }} }}"#,
            collection = collection,
            field = field
        );
        match access.execute(&query).await {
            Ok(_) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": true,
            })),
            Err(error) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": false,
                "error": error.to_string(),
            })),
        }
    }
    results
}

fn diagnose_tool_ceiling(init_config: Option<&StoredInitConfig>) -> Value {
    match init_config {
        Some(config) => {
            let tool_root = config.tool_root.as_deref();
            let ok = match config.tool_ceiling {
                ToolCeilingArg::Readonly | ToolCeilingArg::Readwrite => tool_root
                    .map(Path::new)
                    .map(|path| path.is_dir())
                    .unwrap_or(false),
                ToolCeilingArg::MetaOnly => true,
            };
            let error = if ok {
                None
            } else {
                Some(
                    "readonly/readwrite tool ceiling requires an existing tool_root directory"
                        .to_string(),
                )
            };
            json!({
                "ok": ok,
                "tool_ceiling": format_tool_ceiling(config.tool_ceiling),
                "tool_root": config.tool_root,
                "error": error,
            })
        }
        None => json!({
            "ok": true,
            "error": null,
            "note": "no local init.json found; tool ceiling is unknown until `defra-agent init` runs"
        }),
    }
}

async fn diagnose_backends(bundle: &ConfigExportBundle) -> Vec<Value> {
    let mut models_by_backend = std::collections::BTreeMap::<String, Vec<String>>::new();
    for behavior in &bundle.agent_behaviors {
        let Some(backend_id) = behavior.get("backend_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(model_name) = behavior.get("model_name").and_then(Value::as_str) else {
            continue;
        };
        if backend_id.trim().is_empty() || model_name.trim().is_empty() {
            continue;
        }
        models_by_backend
            .entry(backend_id.to_string())
            .or_default()
            .push(model_name.to_string());
    }
    for models in models_by_backend.values_mut() {
        models.sort();
        models.dedup();
    }

    let mut reports = Vec::new();
    let present_backend_ids = bundle
        .inference_backends
        .iter()
        .filter_map(|backend| backend.get("backend_id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    for backend in &bundle.inference_backends {
        reports.push(
            diagnose_backend(
                backend,
                models_by_backend
                    .get(
                        backend
                            .get("backend_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                    .cloned()
                    .unwrap_or_default(),
            )
            .await,
        );
    }
    for backend_id in models_by_backend.keys() {
        if !present_backend_ids.contains(backend_id) {
            reports.push(json!({
                "backend_id": backend_id,
                "ok": false,
                "error": format!("referenced backend {} is missing", backend_id),
                "required_models": models_by_backend.get(backend_id).cloned().unwrap_or_default(),
            }));
        }
    }
    reports.sort_by(|left, right| {
        let left_key = left
            .get("backend_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_key = right
            .get("backend_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left_key.cmp(right_key)
    });
    reports
}

async fn diagnose_backend(backend: &Value, required_models: Vec<String>) -> Value {
    let backend_id = backend
        .get("backend_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let provider_kind = match BackendProviderKind::parse_optional(
        backend.get("provider_kind").and_then(Value::as_str),
    ) {
        Ok(kind) => kind,
        Err(error) => {
            return json!({
                "backend_id": backend_id,
                "ok": false,
                "provider_kind": backend.get("provider_kind"),
                "error": error.to_string(),
            });
        }
    };
    let endpoint = backend
        .get("endpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let enabled = backend
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let probe_status = backend
        .get("probe_status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let api_key_env_var = backend
        .get("api_key_env_var")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let raw_api_key = backend
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let mut ok = enabled && probe_status == "healthy";
    let mut error = None::<String>;
    let mut discovered_models = Vec::<String>::new();

    let api_key = match (raw_api_key.as_ref(), api_key_env_var.as_deref()) {
        (Some(raw), Some(name)) => {
            ok = false;
            error = Some(format!(
                "backend {} sets both raw api_key and api_key_env_var {}",
                backend_id, name
            ));
            Some(raw.clone())
        }
        (Some(raw), None) => Some(raw.clone()),
        (None, Some(name)) => match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => Some(value),
            _ => {
                ok = false;
                error = Some(format!(
                    "required backend API key env var {} is not set",
                    name
                ));
                None
            }
        },
        (None, None) => None,
    };

    if ok {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(client) => client,
            Err(build_error) => {
                ok = false;
                error = Some(format!("building probe client: {build_error}"));
                return json!({
                    "backend_id": backend_id,
                    "ok": ok,
                    "provider_kind": provider_kind.as_str(),
                    "endpoint": endpoint,
                    "enabled": enabled,
                    "probe_status": probe_status,
                    "api_key": raw_api_key.as_ref().map(|_| "<redacted>"),
                    "api_key_env_var": api_key_env_var,
                    "required_models": required_models,
                    "discovered_models": discovered_models,
                    "error": error,
                });
            }
        };
        match discover_backend_models(&client, provider_kind, &endpoint, api_key.as_deref()).await {
            Ok(models) => {
                discovered_models = models;
                let missing_models = required_models
                    .iter()
                    .filter(|model| {
                        !discovered_models
                            .iter()
                            .any(|candidate| candidate == *model)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !missing_models.is_empty() {
                    ok = false;
                    error = Some(format!(
                        "backend {} is missing required models: {}",
                        backend_id,
                        missing_models.join(", ")
                    ));
                }
            }
            Err(request_error) => {
                ok = false;
                error = Some(format!("backend discovery failed: {}", request_error));
            }
        }
    }

    json!({
        "backend_id": backend_id,
        "ok": ok,
        "provider_kind": provider_kind.as_str(),
        "endpoint": endpoint,
        "enabled": enabled,
        "probe_status": probe_status,
        "api_key": raw_api_key.as_ref().map(|_| "<redacted>"),
        "api_key_env_var": api_key_env_var,
        "required_models": required_models,
        "discovered_models": discovered_models,
        "error": error,
    })
}

async fn post_graphql(graphql: &str, query: &str) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let response = client
        .post(graphql)
        .json(&json!({ "query": query }))
        .send()
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to post GraphQL to {graphql}: {error}\n{}",
                graphql_diagnostic_hint(graphql)
            )
        })?;
    let value: serde_json::Value = response.json().await.map_err(|error| {
        anyhow::anyhow!(
            "failed to decode GraphQL response from {graphql}: {error}\n{}",
            graphql_diagnostic_hint(graphql)
        )
    })?;
    if let Some(errors) = value.get("errors") {
        anyhow::bail!(
            "graphql returned errors from {graphql}: {errors}\n{}",
            graphql_diagnostic_hint(graphql)
        );
    }
    Ok(value)
}

fn extract_mutation_doc_id(response: &Value, collection_name: &str) -> Result<String> {
    let data = response
        .get("data")
        .ok_or_else(|| anyhow::anyhow!("graphql response missing data: {response}"))?;
    for field_name in [
        format!("upsert_{collection_name}"),
        format!("update_{collection_name}"),
        format!("create_{collection_name}"),
        format!("add_{collection_name}"),
    ] {
        if let Some(doc_id) = data
            .get(&field_name)
            .and_then(|value| value.get("_docID"))
            .and_then(Value::as_str)
        {
            return Ok(doc_id.to_string());
        }
        if let Some(doc_id) = data
            .get(&field_name)
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(Value::as_str)
        {
            return Ok(doc_id.to_string());
        }
    }
    anyhow::bail!("graphql mutation returned no _docID for {collection_name}: {response}");
}

async fn initialize_runtime_home(
    access: &ConfigAccess,
    args: &InitArgs,
    agent_did: &str,
) -> Result<InitSummary> {
    let ConfigAccess::Local(node) = access else {
        anyhow::bail!("init requires local DefraDB access");
    };
    let explicit_backend_id = args
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let explicit_backend_name = args
        .backend_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model_name = args.model_name.trim();
    if model_name.is_empty() {
        anyhow::bail!("--model-name must not be empty");
    }
    let backend = resolve_init_backend_config(args)?;
    let backend_id = explicit_backend_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}-backend", args.agent_name));
    let backend_name = explicit_backend_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| backend_id.clone());
    let existing_principal = load_agent_principal(node, agent_did).await?;
    let default_behavior_id = existing_principal
        .as_ref()
        .and_then(|principal| normalize_optional_string(principal.default_behavior_id.as_deref()))
        .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));
    let existing_default_behavior = load_agent_behavior(node, &default_behavior_id).await?;
    if let Some(behavior) = existing_default_behavior.as_ref() {
        if behavior.agent_did != agent_did {
            anyhow::bail!(
                "AgentBehavior {} belongs to {} not {}",
                default_behavior_id,
                behavior.agent_did,
                agent_did
            );
        }
    }
    let principal_display_name = existing_principal
        .as_ref()
        .and_then(|principal| normalize_optional_string(principal.display_name.as_deref()))
        .unwrap_or_else(|| args.agent_name.clone());
    let principal_enabled = existing_principal
        .as_ref()
        .map(|principal| principal.enabled)
        .unwrap_or(true);
    upsert_agent_principal(
        node,
        agent_did,
        Some(&principal_display_name),
        Some(&default_behavior_id),
        principal_enabled,
    )
    .await?;
    let tool_selection_id = format!("{default_behavior_id}:tools");
    let tool_ceiling = if args.write_tools {
        ToolCeilingArg::Readwrite
    } else {
        ToolCeilingArg::Readonly
    };
    let tool_root = Some(resolve_default_tool_root(args.tool_root.as_deref())?);
    let backend_doc = InferenceBackendUpsertDocument {
        backend_id: backend_id.clone(),
        name: backend_name.clone(),
        provider_kind: backend.provider_kind,
        endpoint: backend.endpoint.clone(),
        api_key: backend.api_key.clone(),
        api_key_env_var: backend.api_key_env_var.clone(),
        max_concurrent: args.max_concurrent,
        max_queue_depth: args.max_queue_depth,
        enabled: true,
        models_on_add: vec![model_name.to_string()],
        models_on_update: Some(vec![model_name.to_string()]),
        probe_status: "healthy".to_string(),
    };
    write_inference_backend_document(access, &backend_doc).await?;

    let tool_selection = standard_tool_selection(agent_did, &tool_selection_id, tool_ceiling);
    write_tool_selection_document(access, &tool_selection).await?;

    let behavior = AgentBehavior {
        behavior_id: default_behavior_id.clone(),
        agent_did: agent_did.to_string(),
        display_name: Some("Default".to_string()),
        system_prompt: Some(standard_system_prompt(tool_ceiling).to_string()),
        backend_id: Some(backend_id.clone()),
        model_name: Some(model_name.to_string()),
        tool_selection_id: Some(tool_selection_id.clone()),
        inference_profile_id: None,
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    write_agent_behavior_document(access, &behavior).await?;

    Ok(InitSummary {
        backend_id,
        backend_name,
        provider_kind: backend.provider_kind,
        endpoint: backend.endpoint,
        api_key: backend.api_key.map(|_| "<redacted>".to_string()),
        api_key_env_var: backend.api_key_env_var,
        model_name: model_name.to_string(),
        max_concurrent: args.max_concurrent,
        max_queue_depth: args.max_queue_depth,
        default_behavior_id,
        tool_selection_id,
        tool_ceiling,
        tool_root: tool_root.map(|path| path.to_string_lossy().to_string()),
        created_principal: existing_principal.is_none(),
        created_default_behavior: existing_default_behavior.is_none(),
    })
}

fn standard_tool_selection(
    agent_did: &str,
    tool_selection_id: &str,
    tool_ceiling: ToolCeilingArg,
) -> ToolSelectionDocument {
    let (display_name, file_tools_mode, bash_mode) = match tool_ceiling {
        ToolCeilingArg::Readwrite => ("Standard Write Tools", "ReadWrite", "Unrestricted"),
        ToolCeilingArg::MetaOnly | ToolCeilingArg::Readonly => {
            ("Standard Read-Only Tools", "ReadOnly", "ReadOnly")
        }
    };
    ToolSelectionDocument {
        selection_id: tool_selection_id.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some(display_name.to_string()),
        enable_file_tools: Some(true),
        file_tools_mode: Some(file_tools_mode.to_string()),
        file_tool_root: None,
        enable_bash: Some(true),
        bash_mode: Some(bash_mode.to_string()),
        cli_tool_names: Some(Vec::new()),
        enable_meta_tools: Some(true),
        delegate_to: Some(Vec::new()),
    }
}

fn standard_system_prompt(tool_ceiling: ToolCeilingArg) -> &'static str {
    match tool_ceiling {
        ToolCeilingArg::Readwrite => STANDARD_READWRITE_SYSTEM_PROMPT,
        ToolCeilingArg::MetaOnly | ToolCeilingArg::Readonly => STANDARD_READONLY_SYSTEM_PROMPT,
    }
}

fn init_next_steps(summary: &InitSummary) -> Vec<String> {
    let mut steps = Vec::new();
    if is_probably_ollama_endpoint(&summary.endpoint) {
        steps.push(format!("ollama pull {}", summary.model_name));
    }
    steps.push("defra-agent server".to_string());
    steps.push("defra-agent chat".to_string());
    steps.push(format!(
        "defra-agent config backend set --graphql http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql --backend-id {} --name {} --endpoint <URL> --max-concurrent {}",
        summary.backend_id, summary.backend_name, summary.max_concurrent
    ));
    steps
}

fn is_probably_ollama_endpoint(endpoint: &str) -> bool {
    endpoint.contains("localhost:11434") || endpoint.contains("127.0.0.1:11434")
}

fn resolve_init_backend_config(args: &InitArgs) -> Result<ResolvedBackendConfig> {
    resolve_backend_config_with_preset(
        args.backend_preset,
        args.inference_endpoint.as_deref(),
        args.provider_kind.as_deref(),
        args.api_key.as_deref(),
        args.api_key_env_var.as_deref(),
        BackendResolutionMode::Init,
    )
}

fn resolve_backend_upsert_config(args: &BackendUpsertArgs) -> Result<ResolvedBackendConfig> {
    resolve_backend_config_with_preset(
        args.backend_preset,
        args.endpoint.as_deref(),
        args.provider_kind.as_deref(),
        args.api_key.as_deref(),
        args.api_key_env_var.as_deref(),
        BackendResolutionMode::ConfigWrite,
    )
}

fn resolve_backend_config_with_preset(
    preset: Option<BackendPresetArg>,
    explicit_endpoint: Option<&str>,
    explicit_provider_kind: Option<&str>,
    explicit_api_key: Option<&str>,
    explicit_api_key_env_var: Option<&str>,
    mode: BackendResolutionMode,
) -> Result<ResolvedBackendConfig> {
    let api_key = normalize_optional_string(explicit_api_key);
    let explicit_api_key_env_var = normalize_optional_string(explicit_api_key_env_var);
    if api_key.is_some() && explicit_api_key_env_var.is_some() {
        anyhow::bail!("provide either --api-key or --api-key-env-var, not both");
    }

    let endpoint = resolve_backend_endpoint(explicit_endpoint, preset, mode)?;
    let provider_kind = resolve_backend_provider_kind(explicit_provider_kind, preset)?;
    let api_key_env_var =
        resolve_backend_api_key_env_var(explicit_api_key_env_var, api_key.is_some(), preset);

    Ok(ResolvedBackendConfig {
        provider_kind,
        endpoint,
        api_key,
        api_key_env_var,
    })
}

fn resolve_backend_endpoint(
    explicit: Option<&str>,
    preset: Option<BackendPresetArg>,
    mode: BackendResolutionMode,
) -> Result<String> {
    normalize_optional_string(explicit)
        .or_else(|| preset.and_then(|candidate| candidate.default_endpoint().map(str::to_string)))
        .or_else(|| {
            (mode == BackendResolutionMode::Init)
                .then(|| std::env::var("INFERENCE_ENDPOINT").ok())
                .flatten()
                .and_then(|value| {
                    let trimmed = value.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                })
        })
        .or_else(|| {
            (mode == BackendResolutionMode::Init).then(|| DEFAULT_INIT_ENDPOINT.to_string())
        })
        .ok_or_else(|| match mode {
            BackendResolutionMode::Init => anyhow::anyhow!(
                "an inference endpoint is required\nNext:\n  1. Pass it explicitly: `defra-agent init http://HOST:PORT/v1 --model-name MODEL`\n  2. Or choose a preset with a default endpoint: `defra-agent init --backend-preset openrouter --model-name MODEL`\n  3. Or set INFERENCE_ENDPOINT before running `defra-agent init`"
            ),
            BackendResolutionMode::ConfigWrite => anyhow::anyhow!(
                "an inference endpoint is required\nNext:\n  1. Pass --endpoint explicitly\n  2. Or choose a preset with a default endpoint, such as --backend-preset openrouter"
            ),
        })
}

fn resolve_backend_provider_kind(
    explicit: Option<&str>,
    preset: Option<BackendPresetArg>,
) -> Result<BackendProviderKind> {
    match normalize_optional_string(explicit) {
        Some(value) => BackendProviderKind::parse_optional(Some(&value)),
        None => Ok(
            preset.map_or_else(BackendProviderKind::default, |candidate| {
                candidate.provider_kind()
            }),
        ),
    }
}

fn resolve_backend_api_key_env_var(
    explicit: Option<String>,
    raw_api_key_present: bool,
    preset: Option<BackendPresetArg>,
) -> Option<String> {
    explicit.or_else(|| {
        (!raw_api_key_present)
            .then(|| preset.and_then(|candidate| candidate.default_api_key_env_var()))
            .flatten()
            .map(ToOwned::to_owned)
    })
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(ToOwned::to_owned)
}

fn peer_id_from_public_addr(value: &str) -> Option<String> {
    let value = normalize_optional_string(Some(value))?;
    parse_public_peer_addr(&value)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
}

fn resolve_p2p_peer_id(
    live_peer_id: Option<&str>,
    shareable_address: Option<&str>,
    listen_addresses: &[String],
    stored_peer_id: Option<&str>,
) -> Option<String> {
    normalize_optional_string(live_peer_id)
        .or_else(|| shareable_address.and_then(peer_id_from_public_addr))
        .or_else(|| {
            listen_addresses
                .iter()
                .find_map(|addr| peer_id_from_public_addr(addr))
        })
        .or_else(|| normalize_optional_string(stored_peer_id))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendResolutionMode {
    Init,
    ConfigWrite,
}

fn default_backend_max_queue_depth() -> i64 {
    100
}

fn resolve_default_tool_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .ok()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or_else(|| anyhow::anyhow!("unable to determine a default tool root for local tools"))
}

fn graphql_string_literal(value: &str) -> String {
    format!(r#""{}""#, escape_graphql_string(value))
}

fn response_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                behavior_id
                session_id
                status
                content
                reasoning
                error_message
                token_count
                progress_seq
                completed_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

fn chat_progress_query(request_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                session_id
                status
                content
                reasoning
                error_message
                progress_seq
                completed_at
            }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_key
                tool_name
                status
                args
                result
                started_at
                completed_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
    )
}

#[derive(Debug, Clone)]
struct SubmittedRequest {
    request_id: String,
    session_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct RequestSubmitOptions {
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
}

#[derive(Debug, Clone)]
struct ChatTurnProgress {
    content: String,
    reasoning: String,
    error_message: Option<String>,
    progress_seq: u64,
    status: String,
}

#[derive(Debug, Clone)]
struct ToolCallProgress {
    tool_call_key: String,
    tool_name: String,
    status: String,
    args: String,
    result: String,
}

async fn create_agent_request(
    graphql: &str,
    agent_did: &str,
    content: &str,
    session_id: Option<&str>,
    behavior_id: Option<&str>,
    options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = session_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let created_at = chrono::Utc::now().to_rfc3339();
    let behavior_field = behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            format!(
                r#"
                behavior_id: "{}","#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_default();
    let request_override_fields = vec![
        optional_f64_field("temperature", options.temperature),
        optional_f64_field("top_p", options.top_p),
        optional_i64_field("top_k", options.top_k),
        optional_i64_field("max_tokens", options.max_tokens),
        options
            .metadata
            .as_ref()
            .map(|metadata| format!(r#"metadata: "{}""#, escape_graphql_string(metadata))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                ");
    let request_override_fields = if request_override_fields.is_empty() {
        String::new()
    } else {
        format!("{request_override_fields},\n                ")
    };
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                {behavior_field}
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "{content}",
                {request_override_fields}status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(&request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_field = behavior_field,
        session_id = escape_graphql_string(&session_id),
        content = escape_graphql_string(content),
        request_override_fields = request_override_fields,
    );
    post_graphql(graphql, &mutation).await?;

    Ok(SubmittedRequest {
        request_id,
        session_id,
        agent_did: agent_did.to_string(),
        behavior_id: behavior_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        temperature: options.temperature,
        top_p: options.top_p,
        top_k: options.top_k,
        max_tokens: options.max_tokens,
        metadata: options.metadata,
    })
}

async fn submit_chat_turn(
    graphql: &str,
    agent_did: &str,
    session_id: &str,
    behavior_id: Option<&str>,
    content: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<(SubmittedRequest, Value)> {
    let existing_tool_calls = load_existing_tool_call_keys(graphql, session_id).await?;
    let submitted = create_agent_request(
        graphql,
        agent_did,
        content,
        Some(session_id),
        behavior_id,
        RequestSubmitOptions::default(),
    )
    .await?;
    let response = stream_turn_progress(
        graphql,
        &submitted,
        existing_tool_calls,
        timeout_secs,
        poll_secs,
    )
    .await?;
    Ok((submitted, response))
}

async fn submit_chat_turn_json(
    graphql: &str,
    agent_did: &str,
    session_id: &str,
    behavior_id: Option<&str>,
    content: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<Value> {
    let submitted = create_agent_request(
        graphql,
        agent_did,
        content,
        Some(session_id),
        behavior_id,
        RequestSubmitOptions::default(),
    )
    .await?;
    let response =
        wait_for_terminal_response(graphql, &submitted.request_id, timeout_secs, poll_secs)
            .await
            .with_context(|| format!("waiting for AgentResponse {}", submitted.request_id))?;
    Ok(chat_turn_output(&submitted, response))
}

fn resolve_home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(default_home_dir)
}

fn default_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".defra-agent")
}

fn default_data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("data")
}

fn init_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(INIT_CONFIG_FILE_NAME)
}

fn runtime_state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_STATE_FILE_NAME)
}

fn write_init_config(home_dir: &Path, state: &StoredInitConfig) -> Result<()> {
    fs::create_dir_all(home_dir)
        .with_context(|| format!("creating home directory {}", home_dir.display()))?;
    let path = init_config_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).context("encoding local init config JSON")?;
    fs::write(&path, contents)
        .with_context(|| format!("writing init config {}", path.display()))?;
    Ok(())
}

fn read_init_config(home_dir: &Path) -> Result<Option<StoredInitConfig>> {
    let path = init_config_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).with_context(|| format!("reading init config {}", path.display()))?;
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding init config {}", path.display()))?;
    Ok(Some(state))
}

fn write_runtime_state(home_dir: &Path, state: &StoredRuntimeState) -> Result<()> {
    fs::create_dir_all(home_dir)
        .with_context(|| format!("creating home directory {}", home_dir.display()))?;
    let path = runtime_state_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).context("encoding local runtime state JSON")?;
    fs::write(&path, contents)
        .with_context(|| format!("writing runtime state {}", path.display()))?;
    Ok(())
}

fn read_runtime_state(home_dir: &Path) -> Result<Option<StoredRuntimeState>> {
    let path = runtime_state_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).with_context(|| format!("reading runtime state {}", path.display()))?;
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding runtime state {}", path.display()))?;
    Ok(Some(state))
}

fn clear_runtime_state(home_dir: &Path) -> Result<bool> {
    let path = runtime_state_path(home_dir);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("removing stale runtime state {}", path.display()))?;
        return Ok(true);
    }
    Ok(false)
}

fn resolve_graphql_endpoint(explicit: Option<&str>, home: Option<&Path>) -> Result<String> {
    if let Some(graphql) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(graphql.to_string());
    }

    let home_dir = resolve_home_dir(home);
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return Ok(runtime_state.graphql);
    }

    Ok(format!(
        "http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"
    ))
}

fn resolve_agent_did(home: Option<&Path>, explicit: Option<&str>) -> Result<String> {
    if let Some(agent_did) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(agent_did.to_string());
    }

    let home_dir = resolve_home_dir(home);
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return Ok(runtime_state.agent_did);
    }
    if let Some(init_config) = read_init_config(&home_dir)? {
        return Ok(init_config.agent_did);
    }

    Ok(format!("did:defra-agent:{DEFAULT_AGENT_NAME}"))
}

fn resolve_request_id(positional: Option<&str>, flag: Option<&str>) -> Result<String> {
    let positional = positional.map(str::trim).filter(|value| !value.is_empty());
    let flag = flag.map(str::trim).filter(|value| !value.is_empty());
    match (positional, flag) {
        (Some(positional), Some(flag)) if positional != flag => {
            anyhow::bail!(
                "conflicting request ids provided: positional={} and --request-id={}\nNext:\n  1. Pass the request id once: `defra-agent show response REQUEST_ID`\n  2. Or use `--request-id REQUEST_ID`, but not both",
                positional,
                flag
            );
        }
        (Some(request_id), _) | (_, Some(request_id)) => Ok(request_id.to_string()),
        (None, None) => anyhow::bail!(
            "missing request id\nNext:\n  1. Pass it positionally: `defra-agent show response REQUEST_ID`\n  2. Or use `--request-id REQUEST_ID`"
        ),
    }
}

fn default_key_path(home_dir: &Path, agent_name: &str) -> PathBuf {
    home_dir.join("keys").join(format!("{agent_name}.key"))
}

fn display_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(addr) if addr == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
}

fn require_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--{field} must not be empty");
    }
    Ok(trimmed)
}

fn nullable_string_field(name: &str, value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

fn graphql_bool_literal(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn normalize_optional_rfc3339(value: Option<&str>) -> Result<Option<String>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw)
                .with_context(|| format!("parsing RFC3339 timestamp {raw}"))?;
            Ok(Some(
                parsed
                    .with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ))
        }
        None => Ok(None),
    }
}

fn resolve_task_prompt(prompt: Option<&str>, prompt_file: Option<&Path>) -> Result<String> {
    match (prompt, prompt_file) {
        (Some(_), Some(path)) => anyhow::bail!(
            "provide either --prompt or --prompt-file, not both ({})",
            path.display()
        ),
        (Some(prompt), None) => Ok(require_non_empty("prompt", prompt)?.to_string()),
        (None, Some(path)) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("reading task prompt from {}", path.display()))?;
            Ok(require_non_empty("prompt-file", &prompt)?.to_string())
        }
        (None, None) => anyhow::bail!("a task prompt is required; pass --prompt or --prompt-file"),
    }
}

fn resolve_chat_message(message: &[String], message_file: Option<&Path>) -> Result<Option<String>> {
    if !message.is_empty() && message_file.is_some() {
        anyhow::bail!("provide either MESSAGE or --message-file, not both");
    }
    if !message.is_empty() {
        return Ok(Some(
            require_non_empty("message", &message.join(" "))?.to_string(),
        ));
    }
    if let Some(path) = message_file {
        let message = fs::read_to_string(path)
            .with_context(|| format!("reading chat message from {}", path.display()))?;
        return Ok(Some(
            require_non_empty("message-file", &message)?.to_string(),
        ));
    }
    Ok(None)
}

fn resolve_request_content(content: Option<&str>, content_file: Option<&Path>) -> Result<String> {
    match (content, content_file) {
        (Some(_), Some(path)) => anyhow::bail!(
            "provide either --content or --content-file, not both ({})",
            path.display()
        ),
        (Some(content), None) => Ok(require_non_empty("content", content)?.to_string()),
        (None, Some(path)) => {
            let content = fs::read_to_string(path)
                .with_context(|| format!("reading request content from {}", path.display()))?;
            Ok(require_non_empty("content-file", &content)?.to_string())
        }
        (None, None) => {
            anyhow::bail!("request content is required; pass --content or --content-file")
        }
    }
}

async fn resolve_scheduled_task_behavior_id(
    graphql: &str,
    agent_did: &str,
    explicit_behavior_id: Option<&str>,
) -> Result<String> {
    let behavior_id = match explicit_behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(behavior_id) => behavior_id.to_string(),
        None => load_default_behavior_id_for_agent(graphql, agent_did).await?,
    };

    ensure_behavior_belongs_to_agent(graphql, agent_did, &behavior_id).await?;
    Ok(behavior_id)
}

async fn load_default_behavior_id_for_agent(graphql: &str, agent_did: &str) -> Result<String> {
    let query = format!(
        r#"{{
            AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                default_behavior_id
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let response = post_graphql(graphql, &query).await?;
    let principal = first_graphql_row(&response, "AgentPrincipal")
        .with_context(|| format!("loading AgentPrincipal for {agent_did}"))?;
    principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("AgentPrincipal {agent_did} has no default_behavior_id"))
}

async fn ensure_behavior_belongs_to_agent(
    graphql: &str,
    agent_did: &str,
    behavior_id: &str,
) -> Result<()> {
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                limit: 1
            ) {{
                behavior_id
                agent_did
            }}
        }}"#,
        behavior_id = escape_graphql_string(behavior_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let behavior = first_graphql_row(&response, "AgentBehavior")
        .with_context(|| format!("loading AgentBehavior {behavior_id}"))?;
    let owner = behavior
        .get("agent_did")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("AgentBehavior {behavior_id} is missing agent_did"))?;
    if owner != agent_did {
        anyhow::bail!(
            "AgentBehavior {} belongs to {} not {}",
            behavior_id,
            owner,
            agent_did
        );
    }
    Ok(())
}

fn first_graphql_row<'a>(response: &'a Value, collection_name: &str) -> Result<&'a Value> {
    response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow::anyhow!("graphql returned no rows for {collection_name}"))
}

fn optional_i64_field(name: &str, value: Option<i64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

fn optional_f64_field(name: &str, value: Option<f64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

fn optional_bool_field(name: &str, value: Option<bool>) -> Option<String> {
    value.map(|value| format!("{name}: {}", graphql_bool_literal(value)))
}

fn optional_string_field(name: &str, value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(r#"{name}: "{}""#, escape_graphql_string(value)))
}

fn string_list_field(name: &str, values: &[String]) -> Option<String> {
    Some(format!(
        "{name}: [{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn parse_cli_tool_arg(value: &str) -> Result<defra_agent::CliToolConfig> {
    let (name, path) = value
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("--cli-tool must be NAME=/absolute/path"))?;
    let name = name.trim();
    let path = path.trim();
    if name.is_empty() || path.is_empty() {
        anyhow::bail!("--cli-tool must be NAME=/absolute/path");
    }

    Ok(cli_tool(
        name,
        PathBuf::from(path),
        format!("Run the approved {name} CLI."),
    ))
}

fn normalize_file_tools_mode(enabled: bool, explicit: Option<&str>) -> Result<String> {
    let value = if enabled {
        explicit
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ReadOnly")
    } else {
        "Off"
    };
    FileToolMode::parse(value)?;
    Ok(value.to_string())
}

fn normalize_bash_mode(enabled: bool, explicit: Option<&str>) -> Result<String> {
    let value = if enabled {
        explicit
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ReadOnly")
    } else {
        "Off"
    };
    BashMode::parse(value)?;
    Ok(value.to_string())
}

fn format_tool_ceiling(value: ToolCeilingArg) -> &'static str {
    match value {
        ToolCeilingArg::MetaOnly => "meta-only",
        ToolCeilingArg::Readonly => "readonly",
        ToolCeilingArg::Readwrite => "readwrite",
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn write_json_output_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    let contents =
        serde_json::to_vec_pretty(value).context("encoding JSON output for output file")?;
    fs::write(path, contents)
        .with_context(|| format!("writing JSON output file {}", path.display()))?;
    Ok(())
}

fn write_text_output_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    fs::write(path, content)
        .with_context(|| format!("writing text output file {}", path.display()))?;
    Ok(())
}

fn response_text_content(response: &Value) -> &str {
    response
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn chat_turn_output(submitted: &SubmittedRequest, response: Value) -> Value {
    json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "agent_did": submitted.agent_did,
        "behavior_id": submitted.behavior_id,
        "response": response,
    })
}

async fn wait_for_terminal_response(
    graphql: &str,
    request_id: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<serde_json::Value> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut last_progress_signature: Option<String> = None;

    loop {
        let query = response_query(request_id);
        let response = post_graphql(graphql, &query).await?;
        let rows = response
            .pointer("/data/AgentResponse")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if let Some(row) = rows.first() {
            let signature = serde_json::to_string(row)
                .context("serializing AgentResponse progress row for timeout tracking")?;
            if last_progress_signature.as_deref() != Some(signature.as_str()) {
                last_progress_signature = Some(signature);
                last_progress_at = tokio::time::Instant::now();
            }

            let status = row
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if matches!(status, "complete" | "error") {
                return Ok(row.clone());
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {request_id} after {timeout_secs}s of inactivity\n{}",
                request_diagnostic_hint(request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

fn dangerously_overwrite_home(home_dir: &Path) -> Result<()> {
    if !home_dir.exists() {
        return Ok(());
    }

    if home_dir.as_os_str().is_empty() || home_dir == Path::new("/") {
        anyhow::bail!("refusing to dangerously overwrite {}", home_dir.display());
    }
    if let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) {
        if home_dir == user_home {
            anyhow::bail!(
                "refusing to dangerously overwrite the user home directory {}; pass a dedicated defra-agent home instead",
                home_dir.display()
            );
        }
    }

    fs::remove_dir_all(home_dir)
        .with_context(|| format!("dangerously overwriting {}", home_dir.display()))?;
    Ok(())
}

async fn load_existing_tool_call_keys(
    graphql: &str,
    session_id: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }}
            ) {{
                tool_call_key
                status
            }}
        }}"#,
        session_id = escape_graphql_string(session_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row.get("tool_call_key")?.as_str()?.to_string(),
                row.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect())
}

async fn stream_turn_progress(
    graphql: &str,
    submitted: &SubmittedRequest,
    mut known_tool_calls: std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<Value> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut latest_content = String::new();
    let mut latest_reasoning = String::new();
    let mut latest_progress_seq = 0;
    let mut latest_error_message: Option<String> = None;
    let mut thinking_printed = false;

    loop {
        let query = chat_progress_query(&submitted.request_id, &submitted.session_id);
        let response = post_graphql(graphql, &query).await?;

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool in tool_rows
            .into_iter()
            .filter_map(|row| decode_tool_call_progress(&row))
        {
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            if previous_status.as_deref() == Some(tool.status.as_str()) {
                continue;
            }
            known_tool_calls.insert(tool.tool_call_key.clone(), tool.status.clone());
            last_progress_at = tokio::time::Instant::now();
            if previous_status.is_none() && matches!(tool.status.as_str(), "completed" | "error") {
                println!(
                    "[tool] {} {}",
                    tool.tool_name,
                    format_tool_args_preview(&tool.args)
                );
            }
            println!("{}", format_tool_progress_line(&tool));
            io::stdout().flush()?;
        }

        let response_row = response
            .pointer("/data/AgentResponse")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();
        if let Some(progress) = response_row
            .as_ref()
            .and_then(|row| decode_chat_turn_progress(row))
        {
            if progress.progress_seq > latest_progress_seq
                || progress.content != latest_content
                || progress.reasoning != latest_reasoning
                || progress.error_message != latest_error_message
            {
                last_progress_at = tokio::time::Instant::now();
            }
            if !thinking_printed
                && progress.status == "streaming"
                && progress.content.is_empty()
                && !progress.reasoning.trim().is_empty()
            {
                println!("[thinking]");
                io::stdout().flush()?;
                thinking_printed = true;
            }
            latest_progress_seq = progress.progress_seq;
            latest_error_message = progress.error_message.clone();
            latest_content = progress.content.clone();
            latest_reasoning = progress.reasoning.clone();

            if matches!(progress.status.as_str(), "complete" | "error") {
                if !progress.content.trim().is_empty() {
                    println!("{}", progress.content);
                    io::stdout().flush()?;
                }
                if progress.status == "error" {
                    if let Some(error_message) = progress
                        .error_message
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        if !progress.content.contains(error_message) {
                            println!("[agent error] {error_message}");
                            println!(
                                "[inspect] defra-agent show response {}",
                                submitted.request_id
                            );
                            io::stdout().flush()?;
                        }
                    } else {
                        println!(
                            "[inspect] defra-agent show response {}",
                            submitted.request_id
                        );
                        io::stdout().flush()?;
                    }
                }
                return Ok(response_row.unwrap_or(Value::Null));
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity\n{}",
                submitted.request_id,
                timeout_secs,
                request_diagnostic_hint(&submitted.request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

fn is_probably_local_graphql_endpoint(graphql: &str) -> bool {
    let graphql = graphql.trim();
    graphql.contains("127.0.0.1") || graphql.contains("localhost")
}

fn graphql_diagnostic_hint(graphql: &str) -> String {
    if is_probably_local_graphql_endpoint(graphql) {
        "Next:\n  1. If this home is not initialized, run `defra-agent init`\n  2. Start the runtime with `defra-agent server`\n  3. Inspect it with `defra-agent status`".to_string()
    } else {
        format!(
            "Next:\n  1. Verify the GraphQL endpoint {graphql}\n  2. Retry with `--graphql {graphql}` or point the command at the correct runtime"
        )
    }
}

fn request_diagnostic_hint(request_id: &str) -> String {
    format!(
        "Next:\n  1. Run `defra-agent show request {request_id}`\n  2. Run `defra-agent show response {request_id}`\n  3. Inspect the runtime with `defra-agent status`"
    )
}

fn server_start_failure_hint(home_dir: &Path) -> String {
    format!(
        "Next:\n  1. For the default local backend, run `ollama pull {DEFAULT_INIT_MODEL_NAME}` and make sure Ollama is listening on {DEFAULT_INIT_ENDPOINT}\n  2. Point the backend elsewhere with `defra-agent config backend set --graphql http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql --backend-id <ID> --name <NAME> --endpoint <URL> --max-concurrent 2`\n  3. Inspect the initialized home at {}\n  4. If persisted runtime state is stale, run `defra-agent reset --home {}`",
        init_config_path(home_dir).display(),
        home_dir.display()
    )
}

fn decode_chat_turn_progress(row: &Value) -> Option<ChatTurnProgress> {
    Some(ChatTurnProgress {
        content: row.get("content")?.as_str()?.to_string(),
        reasoning: row
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        error_message: row
            .get("error_message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        progress_seq: row.get("progress_seq").and_then(Value::as_u64).unwrap_or(0),
        status: row.get("status")?.as_str()?.to_string(),
    })
}

fn decode_tool_call_progress(row: &Value) -> Option<ToolCallProgress> {
    Some(ToolCallProgress {
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        status: row.get("status")?.as_str()?.to_string(),
        args: row
            .get("args")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result: row
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn format_tool_progress_line(tool: &ToolCallProgress) -> String {
    match tool.status.as_str() {
        "completed" => match preview_compact_text(&tool.result) {
            Some(result) => format!(
                "[tool done] {} {} => {}",
                tool.tool_name,
                format_tool_args_preview(&tool.args),
                result
            ),
            None => format!(
                "[tool done] {} {}",
                tool.tool_name,
                format_tool_args_preview(&tool.args)
            ),
        },
        "error" => format!(
            "[tool error] {} {} => {}",
            tool.tool_name,
            format_tool_args_preview(&tool.args),
            preview_compact_text(&tool.result).unwrap_or_else(|| "-".to_string())
        ),
        _ => format!(
            "[tool] {} {}",
            tool.tool_name,
            format_tool_args_preview(&tool.args)
        ),
    }
}

fn format_tool_args_preview(value: &str) -> String {
    preview_compact_text(value)
        .map(|preview| format!("({preview})"))
        .unwrap_or_default()
}

fn preview_compact_text(value: &str) -> Option<String> {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = if trimmed.chars().count() > 120 {
        format!("{}...", trimmed.chars().take(120).collect::<String>())
    } else {
        trimmed.to_string()
    };
    Some(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_with_rows(
        agent_behaviors: Vec<Value>,
        tool_selections: Vec<Value>,
        inference_backends: Vec<Value>,
        inference_profiles: Vec<Value>,
    ) -> ConfigExportBundle {
        ConfigExportBundle {
            format: CONFIG_EXPORT_FORMAT.to_string(),
            agent_did: "did:defra-agent:test".to_string(),
            exported_at: "2026-04-14T00:00:00Z".to_string(),
            access_mode: "graphql".to_string(),
            agent_principal: None,
            agent_behaviors,
            tool_selections,
            inference_backends,
            inference_profiles,
            tool_service_registries: Vec::new(),
            scheduled_tasks: Vec::new(),
        }
    }

    #[test]
    fn collect_unavailable_behaviors_from_bundle_reports_config_and_backend_issues() {
        let bundle = bundle_with_rows(
            vec![
                json!({
                    "behavior_id": "did:defra-agent:test:default",
                    "enabled": true,
                    "backend_id": "",
                    "tool_selection_id": "",
                    "inference_profile_id": ""
                }),
                json!({
                    "behavior_id": "did:defra-agent:test:ops",
                    "enabled": true,
                    "backend_id": "backend-unhealthy",
                    "tool_selection_id": "",
                    "inference_profile_id": ""
                }),
                json!({
                    "behavior_id": "did:defra-agent:test:broken-tools",
                    "enabled": true,
                    "backend_id": "backend-healthy",
                    "tool_selection_id": "missing-tools",
                    "inference_profile_id": ""
                }),
            ],
            Vec::new(),
            vec![
                json!({
                    "backend_id": "backend-unhealthy",
                    "provider_kind": "OpenAiCompatible",
                    "enabled": true,
                    "probe_status": "unknown"
                }),
                json!({
                    "backend_id": "backend-healthy",
                    "provider_kind": "OpenAiCompatible",
                    "enabled": true,
                    "probe_status": "healthy"
                }),
            ],
            Vec::new(),
        );

        let unavailable = collect_unavailable_behaviors_from_bundle(&bundle);
        assert_eq!(
            unavailable.get("did:defra-agent:test:default"),
            Some(&"behavior did:defra-agent:test:default has no backend binding".to_string())
        );
        assert_eq!(
            unavailable.get("did:defra-agent:test:ops"),
            Some(
                &"behavior did:defra-agent:test:ops backend backend-unhealthy is unavailable (enabled=true probe_status=unknown)".to_string()
            )
        );
        assert_eq!(
            unavailable.get("did:defra-agent:test:broken-tools"),
            Some(
                &"behavior did:defra-agent:test:broken-tools references missing tool selection missing-tools".to_string()
            )
        );
    }

    #[test]
    fn sanitize_inference_backend_drops_deprecated_capability_fields() {
        let input = json!({
            "backend_id": "local",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "api_key_env_var": null,
            "max_concurrent": 1,
            "max_queue_depth": 100,
            "enabled": true,
            "supports_tool_calls": true,
            "supports_streaming": true,
            "supports_structured_outputs": false,
            "supports_json_schema": false,
            "context_window": 32768,
            "max_output_tokens": 4096,
            "last_probe": "2026-04-15T00:00:00Z",
            "models": ["test-model"],
            "probe_status": "healthy"
        });

        let out = sanitize_import_document("InferenceBackend", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        for field in [
            "supports_tool_calls",
            "supports_streaming",
            "supports_structured_outputs",
            "supports_json_schema",
            "context_window",
            "max_output_tokens",
            "last_probe",
        ] {
            assert!(!obj.contains_key(field), "{field} should be stripped");
        }
        assert_eq!(obj.get("backend_id").and_then(Value::as_str), Some("local"));
    }

    #[test]
    fn read_config_import_bundle_migrates_v1_backend_capability_fields() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("config.json");
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "format": CONFIG_EXPORT_FORMAT_V1,
                "agent_did": "did:defra-agent:test",
                "exported_at": "2026-04-15T00:00:00Z",
                "access_mode": "local",
                "agent_principal": null,
                "agent_behaviors": [],
                "tool_selections": [],
                "inference_backends": [{
                    "backend_id": "local",
                    "name": "Local",
                    "provider_kind": "OpenAiCompatible",
                    "endpoint": "http://127.0.0.1:11434/v1",
                    "api_key": null,
                    "api_key_env_var": null,
                    "max_concurrent": 1,
                    "max_queue_depth": 100,
                    "enabled": true,
                    "supports_tool_calls": true,
                    "supports_streaming": true,
                    "supports_structured_outputs": false,
                    "supports_json_schema": false,
                    "models": ["test-model"],
                    "probe_status": "healthy"
                }],
                "inference_profiles": [],
                "tool_service_registries": [],
                "scheduled_tasks": []
            }))
            .unwrap(),
        )
        .unwrap();

        let bundle = read_config_import_bundle(Some(&path)).unwrap();
        validate_config_import_bundle(&bundle).unwrap();
        assert_eq!(bundle.format, CONFIG_EXPORT_FORMAT);
        let backend = bundle.inference_backends[0].as_object().unwrap();
        assert!(!backend.contains_key("supports_tool_calls"));
        assert!(!backend.contains_key("supports_streaming"));
        assert!(!backend.contains_key("supports_structured_outputs"));
        assert!(!backend.contains_key("supports_json_schema"));
    }

    #[test]
    fn sanitize_tool_service_registry_defaults_status_online_when_absent() {
        let input = json!({
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "tailscale_ip": "100.69.4.79",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("online"));
    }

    #[test]
    fn sanitize_tool_service_registry_fills_status_when_null() {
        let input = json!({
            "service_id": "observability-mcp",
            "status": null,
            "hostname": "studio-1",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("online"));
    }

    #[test]
    fn sanitize_tool_service_registry_preserves_explicit_status() {
        let input = json!({
            "service_id": "observability-mcp",
            "status": "offline",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("offline"));
    }

    #[test]
    fn sanitize_tool_service_registry_normalizes_address_fields_for_storage() {
        let input = json!({
            "service_id": "observability-mcp",
            "hostname": null,
            "tailscale_ip": " 100.69.4.79 ",
            "lan_ip": null,
            "mcp_port": 9201,
            "mcp_path": "mcp"
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("hostname").and_then(|v| v.as_str()), Some(""));
        assert_eq!(obj.get("lan_ip").and_then(|v| v.as_str()), Some(""));
        assert_eq!(
            obj.get("tailscale_ip").and_then(|v| v.as_str()),
            Some("100.69.4.79")
        );
        assert_eq!(obj.get("mcp_path").and_then(|v| v.as_str()), Some("/mcp"));
    }

    #[test]
    fn sanitize_tool_service_registry_defaults_mcp_path() {
        let input = json!({
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("mcp_path").and_then(|v| v.as_str()), Some("/mcp"));
    }

    #[test]
    fn sanitize_tool_service_registry_still_strips_runtime_owned_fields() {
        let input = json!({
            "service_id": "observability-mcp",
            "mcp_port": 9201,
            "tools": [{"name": "x", "description": "y"}],
            "version": "1.2.3",
            "updated_at": "2026-04-14T00:00:00Z"
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert!(obj.get("tools").is_none(), "tools should be stripped");
        assert!(obj.get("version").is_none(), "version should be stripped");
        assert!(
            obj.get("updated_at").is_none(),
            "updated_at should be stripped on create"
        );
    }

    #[test]
    fn p2p_collection_profiles_expand_and_dedupe_collection_names() {
        let collections = expand_p2p_collection_args(
            &[
                " AgentRequest ".to_string(),
                "AgentRequest".to_string(),
                "".to_string(),
            ],
            &[
                P2pCollectionProfileArg::ChatRequests,
                P2pCollectionProfileArg::ToolServices,
            ],
        )
        .unwrap();

        assert!(collections.iter().any(|name| name == "AgentRequest"));
        assert!(collections.iter().any(|name| name == "AgentResponse"));
        assert!(collections.iter().any(|name| name == "ToolServiceRegistry"));
        assert_eq!(
            collections
                .iter()
                .filter(|name| name.as_str() == "AgentRequest")
                .count(),
            1
        );
    }

    #[test]
    fn p2p_collection_args_require_collection_or_profile() {
        let error = expand_p2p_collection_args(&[], &[]).unwrap_err();
        assert!(error
            .to_string()
            .contains("provide at least one --collection or --profile"));
    }

    #[test]
    fn p2p_collection_rows_include_human_readable_names_when_known() {
        let mut names_by_id = BTreeMap::new();
        names_by_id.insert("bafk-agent-request".to_string(), "AgentRequest".to_string());
        let rows = p2p_collection_rows(
            &["bafk-agent-request".to_string(), "bafk-unknown".to_string()],
            &names_by_id,
        );

        assert_eq!(rows[0].id, "bafk-agent-request");
        assert_eq!(rows[0].name.as_deref(), Some("AgentRequest"));
        assert_eq!(rows[1].id, "bafk-unknown");
        assert!(rows[1].name.is_none());
    }

    #[test]
    fn p2p_replicator_rows_resolve_collection_names() {
        let mut names_by_id = BTreeMap::new();
        names_by_id.insert("bafk-agent-runtime".to_string(), "AgentRuntime".to_string());
        let rows = p2p_replicator_rows(
            vec![P2pReplicatorRow {
                id: Some("peer-1".to_string()),
                addresses: vec!["iroh://peer-1".to_string()],
                collection_ids: vec!["bafk-agent-runtime".to_string(), "bafk-missing".to_string()],
            }],
            &names_by_id,
        );

        assert_eq!(rows[0].id.as_deref(), Some("peer-1"));
        assert_eq!(rows[0].collection_names, vec!["AgentRuntime"]);
        assert_eq!(rows[0].collection_ids.len(), 2);
    }

    #[test]
    fn resolve_p2p_peer_id_uses_shareable_address_when_identity_is_missing() {
        let peer_id = resolve_p2p_peer_id(
            None,
            Some("127.0.0.1:56000/p2p/peer-alpha"),
            &[],
            Some("persisted-peer"),
        );

        assert_eq!(peer_id.as_deref(), Some("peer-alpha"));
    }

    #[test]
    fn resolve_p2p_peer_id_falls_back_to_listen_or_stored_values() {
        let peer_id = resolve_p2p_peer_id(
            None,
            None,
            &[String::from("127.0.0.1:56000/p2p/peer-beta")],
            Some("persisted-peer"),
        );
        assert_eq!(peer_id.as_deref(), Some("peer-beta"));

        let peer_id = resolve_p2p_peer_id(None, None, &[], Some("persisted-peer"));
        assert_eq!(peer_id.as_deref(), Some("persisted-peer"));
    }
}
