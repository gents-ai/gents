use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    cli_tool, default_behavior_id_for_agent, discover_backend_models, ensure_runtime_schemas,
    AgentBehavior, BackendProviderKind, BashMode, FileToolMode, ToolSelectionDocument,
};
use p2p::iroh::parse_public_peer_addr;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Map, Value};

mod cli;
mod commands;
mod config_writes;
mod desired_state;
mod http;
mod shared;
mod telemetry;

use cli::*;
use shared::*;

use config_writes::{
    write_agent_behavior_document, write_inference_backend_document,
    write_scheduled_task_document, write_tool_selection_document, ConfigAccess,
    InferenceBackendUpsertDocument,
};
use http::version::{NodeIdentityResponse, P2pShareableAddressResponse};

const DEFAULT_AGENT_NAME: &str = "default";
const DEFAULT_INIT_ENDPOINT: &str = "http://localhost:11434/v1";
const DEFAULT_INIT_MODEL_NAME: &str = "gemma4-26b-a4b";
const DEFAULT_HTTP_PORT: u16 = 9191;
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
pub(crate) const CONFIG_EXPORT_FORMAT_V1: &str = "defra-agent-config/v1";
pub(crate) const CONFIG_EXPORT_FORMAT: &str = "defra-agent-config/v2";

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
pub(crate) const EXPORT_AGENT_PRINCIPAL_FIELDS: &str =
    "agent_did display_name default_behavior_id enabled created_at created_by";
pub(crate) const EXPORT_AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled created_at";
pub(crate) const EXPORT_TOOL_SELECTION_FIELDS: &str = "selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode cli_tool_names enable_meta_tools delegate_to";
pub(crate) const EXPORT_INFERENCE_BACKEND_FIELDS: &str =
    "backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status";
pub(crate) const EXPORT_INFERENCE_PROFILE_FIELDS: &str =
    "profile_id display_name context_window max_output_tokens max_turns temperature stream_batch_ms deadline_duration_secs";
pub(crate) const EXPORT_TOOL_SERVICE_REGISTRY_FIELDS: &str =
    "service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path";
pub(crate) const EXPORT_SCHEDULED_TASK_FIELDS: &str =
    "task_id agent_did behavior_id name prompt interval_secs enabled";


#[tokio::main]
async fn main() -> Result<()> {
    let telemetry = telemetry::init(DEFAULT_LOG_FILTER)?;
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init(args) => commands::init::init(args).await,
        Command::Reset(args) => commands::reset::reset(args).await,
        Command::Server(args) => commands::serve::serve(args).await,
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
        Command::Show { command } => match command {
            ShowCommand::Request(args) => request_show(args).await,
            ShowCommand::Response(args) => response_show(args).await,
            ShowCommand::Runtime(args) => show_runtime(args).await,
        },
        Command::Status(args) => status(args).await,
        Command::Diagnose(args) => diagnose(args).await,
        Command::Config { command } => commands::config::dispatch(command).await,
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

pub(crate) async fn resolve_config_access(
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

pub(crate) async fn graphql_rows(
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

pub(crate) async fn graphql_rows_or_empty_if_collection_missing(
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

pub(crate) fn is_collection_missing_error(collection_name: &str, error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains(collection_name)
        && (message.contains("collection not found") || message.contains("Cannot query field"))
}

pub(crate) async fn build_config_export_bundle(
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

pub(crate) async fn build_desired_state_live_bundle(
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

pub(crate) fn live_manifest_from_bundle(
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

pub(crate) fn sort_document_rows(rows: &mut [Value], key: &str) {
    rows.sort_by(|left, right| {
        let left_key = left.get(key).and_then(Value::as_str).unwrap_or_default();
        let right_key = right.get(key).and_then(Value::as_str).unwrap_or_default();
        left_key.cmp(right_key)
    });
}

pub(crate) fn normalize_tool_service_registry_export_rows(rows: &mut [Value]) -> Result<()> {
    for row in rows {
        let object = row
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("ToolServiceRegistry export row must be an object"))?;
        desired_state::normalize_tool_service_registry_storage_fields(object)?;
    }
    Ok(())
}

pub(crate) fn collect_string_field_values(rows: &[Value], field: &str) -> Vec<String> {
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

pub(crate) fn graphql_string_list_literal(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(crate) fn read_config_import_bundle(path: Option<&Path>) -> Result<ConfigExportBundle> {
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

pub(crate) fn validate_config_import_bundle(bundle: &ConfigExportBundle) -> Result<()> {
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

pub(crate) fn migrate_config_import_bundle(bundle: &mut ConfigExportBundle) {
    for backend in &mut bundle.inference_backends {
        if let Some(object) = backend.as_object_mut() {
            desired_state::strip_deprecated_inference_backend_fields(object);
        }
    }
    if bundle.format == CONFIG_EXPORT_FORMAT_V1 {
        bundle.format = CONFIG_EXPORT_FORMAT.to_string();
    }
}

pub(crate) async fn apply_import_collection(
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

pub(crate) fn sanitize_import_document(collection_name: &str, doc: &Value, for_update: bool) -> Result<Value> {
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

pub(crate) fn diff_has_pending_apply(counts: &desired_state::DesiredStateDiffCollectionsCounts) -> bool {
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

pub(crate) fn config_apply_counts_changed(counts: &ConfigApplyCounts) -> bool {
    counts.agent_principal > 0
        || counts.agent_behaviors > 0
        || counts.tool_selections > 0
        || counts.inference_backends > 0
        || counts.inference_profiles > 0
        || counts.tool_service_registries > 0
        || counts.scheduled_tasks > 0
}

pub(crate) fn select_apply_collection_docs(
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

pub(crate) fn select_apply_principal_docs(
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

pub(crate) async fn apply_desired_state_changes(
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

pub(crate) fn graphql_input_literal(value: &Value) -> Result<String> {
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

pub(crate) async fn post_graphql(graphql: &str, query: &str) -> Result<serde_json::Value> {
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

pub(crate) fn extract_mutation_doc_id(response: &Value, collection_name: &str) -> Result<String> {
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

pub(crate) fn normalize_optional_string(value: Option<&str>) -> Option<String> {
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
pub(crate) enum BackendResolutionMode {
    Init,
    ConfigWrite,
}

pub(crate) fn default_backend_max_queue_depth() -> i64 {
    100
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

pub(crate) fn resolve_graphql_endpoint(explicit: Option<&str>, home: Option<&Path>) -> Result<String> {
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

pub(crate) fn resolve_agent_did(home: Option<&Path>, explicit: Option<&str>) -> Result<String> {
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

pub(crate) fn require_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--{field} must not be empty");
    }
    Ok(trimmed)
}

pub(crate) fn nullable_string_field(name: &str, value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

pub(crate) fn graphql_bool_literal(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

pub(crate) fn normalize_optional_rfc3339(value: Option<&str>) -> Result<Option<String>> {
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

pub(crate) fn resolve_task_prompt(prompt: Option<&str>, prompt_file: Option<&Path>) -> Result<String> {
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

pub(crate) async fn resolve_scheduled_task_behavior_id(
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

pub(crate) fn optional_i64_field(name: &str, value: Option<i64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub(crate) fn optional_f64_field(name: &str, value: Option<f64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub(crate) fn optional_bool_field(name: &str, value: Option<bool>) -> Option<String> {
    value.map(|value| format!("{name}: {}", graphql_bool_literal(value)))
}

pub(crate) fn optional_string_field(name: &str, value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(r#"{name}: "{}""#, escape_graphql_string(value)))
}

pub(crate) fn string_list_field(name: &str, values: &[String]) -> Option<String> {
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

pub(crate) fn normalize_file_tools_mode(enabled: bool, explicit: Option<&str>) -> Result<String> {
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

pub(crate) fn normalize_bash_mode(enabled: bool, explicit: Option<&str>) -> Result<String> {
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

pub(crate) fn print_json(value: &Value) -> Result<()> {
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
