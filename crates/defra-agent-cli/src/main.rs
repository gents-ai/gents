use std::fs;
use std::io::{self, BufRead, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    cli_tool, default_behavior_id_for_agent, ensure_agent_principal, ensure_runtime_schemas,
    AgentIdentity, BashMode, DefraAgent, DocumentRuntimeOptions, FileToolMode, McpPool,
    ProcessLifecycleObserver, ProcessLifecycleState, SimpleIdentity, ToolCeiling,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

mod tui;

const DEFAULT_AGENT_NAME: &str = "default";
const DEFAULT_HTTP_PORT: u16 = 9191;
const DEFAULT_LOG_FILTER: &str = concat!(
    "warn,",
    "defra_agent::agent::runtime=info,",
    "defra_agent::agent::daemon=info,",
    "defra_agent::agent::reconcile=info,",
    "defra_agent::session::sessions=info,",
    "defra_agent::streaming=info,",
    "defra_agent::scheduler::loop_impl=info"
);
const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
const BOOTSTRAP_INFERENCE_BACKEND_DEFAULT: &str =
    include_str!("../bootstrap/InferenceBackend/default.gql");
const BOOTSTRAP_TOOL_SELECTION_STANDARD_READONLY: &str =
    include_str!("../bootstrap/ToolSelection/standard-readonly.gql");
const BOOTSTRAP_TOOL_SELECTION_STANDARD_READWRITE: &str =
    include_str!("../bootstrap/ToolSelection/standard-readwrite.gql");
const BOOTSTRAP_AGENT_BEHAVIOR_STANDARD_READONLY: &str =
    include_str!("../bootstrap/AgentBehavior/standard-readonly.gql");
const BOOTSTRAP_AGENT_BEHAVIOR_STANDARD_READWRITE: &str =
    include_str!("../bootstrap/AgentBehavior/standard-readwrite.gql");

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
    about = "Consumer CLI for the defra-agent library"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init(InitArgs),
    #[command(name = "server")]
    Server(ServeArgs),
    Chat(ChatArgs),
    Tui(TuiArgs),
    Show {
        #[command(subcommand)]
        command: ShowCommand,
    },
    Status(StatusArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Request {
        #[command(subcommand)]
        command: RequestCommand,
    },
    Response {
        #[command(subcommand)]
        command: ResponseCommand,
    },
}

#[derive(clap::Args)]
struct InitArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long, hide = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    dangerously_overwrite: bool,
    #[arg(long, default_value = DEFAULT_AGENT_NAME)]
    agent_name: String,
    #[arg(long)]
    key_path: Option<PathBuf>,
    #[arg(value_name = "INFERENCE_ENDPOINT")]
    inference_endpoint: Option<String>,
    #[arg(long)]
    backend_id: Option<String>,
    #[arg(long)]
    backend_name: Option<String>,
    #[arg(long)]
    model_name: String,
    #[arg(long, default_value_t = 1)]
    max_concurrent: i64,
    #[arg(long, default_value_t = false)]
    write_tools: bool,
    #[arg(long)]
    tool_root: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long)]
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
    #[arg(long, value_enum)]
    tool_ceiling: Option<ToolCeilingArg>,
    #[arg(long = "cli-tool")]
    cli_tools: Vec<String>,
    #[arg(long)]
    tool_root: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ChatArgs {
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
    Request(RequestShowArgs),
    Response(ResponseShowArgs),
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
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
    },
    Behavior {
        #[command(subcommand)]
        command: BehaviorCommand,
    },
    Tools {
        #[command(subcommand)]
        command: ToolSelectionCommand,
    },
    Profile {
        #[command(subcommand)]
        command: InferenceProfileCommand,
    },
    Task {
        #[command(subcommand)]
        command: ScheduledTaskCommand,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
enum ToolCeilingArg {
    MetaOnly,
    Readonly,
    Readwrite,
}

#[derive(Debug, Clone, serde::Serialize)]
struct InitSummary {
    backend_id: String,
    backend_name: String,
    endpoint: String,
    model_name: String,
    default_behavior_id: String,
    tool_selection_id: String,
    tool_ceiling: ToolCeilingArg,
    tool_root: Option<String>,
    created_principal: bool,
    created_default_behavior: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredInitConfig {
    home: String,
    agent_name: String,
    agent_did: String,
    key_path: Option<String>,
    backend_id: String,
    backend_name: String,
    endpoint: String,
    model_name: String,
    default_behavior_id: String,
    tool_selection_id: String,
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
}

#[derive(Clone, Copy)]
struct BootstrapMutationTemplate {
    collection: &'static str,
    option: &'static str,
    mutation: &'static str,
}

const STANDARD_READONLY_BOOTSTRAP: &[BootstrapMutationTemplate] = &[
    BootstrapMutationTemplate {
        collection: "InferenceBackend",
        option: "default",
        mutation: BOOTSTRAP_INFERENCE_BACKEND_DEFAULT,
    },
    BootstrapMutationTemplate {
        collection: "ToolSelection",
        option: "standard-readonly",
        mutation: BOOTSTRAP_TOOL_SELECTION_STANDARD_READONLY,
    },
    BootstrapMutationTemplate {
        collection: "AgentBehavior",
        option: "standard-readonly",
        mutation: BOOTSTRAP_AGENT_BEHAVIOR_STANDARD_READONLY,
    },
];

const STANDARD_READWRITE_BOOTSTRAP: &[BootstrapMutationTemplate] = &[
    BootstrapMutationTemplate {
        collection: "InferenceBackend",
        option: "default",
        mutation: BOOTSTRAP_INFERENCE_BACKEND_DEFAULT,
    },
    BootstrapMutationTemplate {
        collection: "ToolSelection",
        option: "standard-readwrite",
        mutation: BOOTSTRAP_TOOL_SELECTION_STANDARD_READWRITE,
    },
    BootstrapMutationTemplate {
        collection: "AgentBehavior",
        option: "standard-readwrite",
        mutation: BOOTSTRAP_AGENT_BEHAVIOR_STANDARD_READWRITE,
    },
];

#[derive(Subcommand)]
enum BackendCommand {
    #[command(name = "set")]
    Set(BackendUpsertArgs),
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
    #[arg(long)]
    endpoint: String,
    #[arg(long)]
    max_concurrent: i64,
    #[arg(long, default_value_t = true)]
    enabled: bool,
    #[arg(long, default_value = "healthy")]
    probe_status: String,
}

#[derive(Subcommand)]
enum RequestCommand {
    Submit(RequestSubmitArgs),
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
    content: String,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    behavior_id: Option<String>,
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
    Show(ResponseShowArgs),
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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER)),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => init(args).await,
        Command::Server(args) => serve(args).await,
        Command::Chat(args) => chat(args).await,
        Command::Tui(args) => tui::run(args).await,
        Command::Show { command } => match command {
            ShowCommand::Request(args) => request_show(args).await,
            ShowCommand::Response(args) => response_show(args).await,
            ShowCommand::Runtime(args) => show_runtime(args).await,
        },
        Command::Status(args) => status(args).await,
        Command::Config { command } => match command {
            ConfigCommand::Backend { command } => match command {
                BackendCommand::Set(args) => backend_set(args).await,
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
        },
        Command::Request { command } => match command {
            RequestCommand::Submit(args) => request_submit(args).await,
            RequestCommand::Show(args) => request_show(args).await,
        },
        Command::Response { command } => match command {
            ResponseCommand::Show(args) => response_show(args).await,
            ResponseCommand::Wait(args) => response_wait(args).await,
        },
    }
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
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .build()
        .await
        .context("building embedded defra node for init")?;
    ensure_runtime_schemas(&node).await?;

    let summary = initialize_runtime_home(&node, &home_dir, &args, identity.did()).await?;
    let stored = StoredInitConfig {
        home: home_dir.to_string_lossy().to_string(),
        agent_name: args.agent_name.clone(),
        agent_did: identity.did().to_string(),
        key_path: Some(key_path.to_string_lossy().to_string()),
        backend_id: summary.backend_id.clone(),
        backend_name: summary.backend_name.clone(),
        endpoint: summary.endpoint.clone(),
        model_name: summary.model_name.clone(),
        default_behavior_id: summary.default_behavior_id.clone(),
        tool_selection_id: summary.tool_selection_id.clone(),
        tool_ceiling: summary.tool_ceiling,
        tool_root: summary.tool_root.clone(),
    };
    write_init_config(&home_dir, &stored)?;
    clear_runtime_state(&home_dir)?;

    let output = json!({
        "status": "initialized",
        "home": home_dir,
        "agent_name": args.agent_name,
        "agent_did": identity.did(),
        "default_behavior_id": summary.default_behavior_id,
        "tool_selection_id": summary.tool_selection_id,
        "tool_ceiling": format_tool_ceiling(summary.tool_ceiling),
        "tool_root": summary.tool_root,
        "init": summary,
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
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_http(defra_node::HttpConfig::with_addr(http_addr))
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
    let effective_tool_root = args.tool_root.clone().or_else(|| {
        init_config
            .as_ref()
            .and_then(|config| config.tool_root.as_ref().map(PathBuf::from))
    });
    let mut tool_ceiling = match effective_tool_ceiling {
        ToolCeilingArg::MetaOnly => ToolCeiling::meta_only(),
        ToolCeilingArg::Readonly => ToolCeiling::readonly(),
        ToolCeilingArg::Readwrite => {
            let root = effective_tool_root.as_ref().ok_or_else(|| {
                anyhow::anyhow!("--tool-root is required when --tool-ceiling readwrite")
            })?;
            ToolCeiling::readwrite(root)
        }
    };
    for cli_tool_arg in &args.cli_tools {
        tool_ceiling = tool_ceiling.with_cli_tool(parse_cli_tool_arg(cli_tool_arg)?);
    }
    let identity = Arc::new(SimpleIdentity::new(&agent_name, &key_path, None));
    let (ready_tx, mut ready_rx) = watch::channel(ProcessLifecycleState::Uninitialized);

    let agent = DefraAgent::from_default_behavior_documents(
        node,
        identity.clone(),
        DocumentRuntimeOptions {
            mcp_pool: McpPool::new(),
            local_hostname: Some(local_hostname),
            tool_ceiling,
            process_state_observer: Some(Arc::new(CliReadyObserver { tx: ready_tx })),
            ..Default::default()
        },
    )
    .await?;
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
    let graphql_url = format!(
        "http://{}:{}/api/v0/graphql",
        display_host(args.http_addr),
        args.http_port
    );

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

    write_runtime_state(
        &home_dir,
        &StoredRuntimeState {
            home: home_dir.to_string_lossy().to_string(),
            graphql: graphql_url.clone(),
            agent_name: agent_name.clone(),
            agent_did: identity.did().to_string(),
            default_behavior_id: default_behavior_id.clone(),
        },
    )?;

    let output = json!({
        "status": "serving",
        "home": home_dir,
        "agent_name": agent_name,
        "agent_did": identity.did(),
        "default_behavior_id": default_behavior_id,
        "tool_ceiling": format_tool_ceiling(effective_tool_ceiling),
        "tool_root": effective_tool_root,
        "runnable_behaviors": runnable_behaviors,
        "unavailable_behaviors": unavailable_behaviors,
        "graphql": graphql_url,
    });
    print_json(&output)?;
    eprintln!(
        "defra-agent server is running. Press Ctrl-C to stop. Run `defra-agent chat` in another terminal."
    );

    run_handle
        .await
        .context("joining defra-agent runtime task")?
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

    if !args.message.is_empty() {
        submit_chat_turn(
            &graphql,
            &agent_did,
            &session_id,
            args.behavior_id.as_deref(),
            &args.message.join(" "),
            args.timeout_secs,
            args.poll_secs,
        )
        .await?;
        return Ok(());
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
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }},
                add: {{
                    backend_id: "{backend_id}",
                    name: "{name}",
                    endpoint: "{endpoint}",
                    max_concurrent: {max_concurrent},
                    enabled: {enabled},
                    models: ["default"],
                    last_probe: "{now}",
                    probe_status: "{probe_status}"
                }},
                update: {{
                    name: "{name}",
                    endpoint: "{endpoint}",
                    max_concurrent: {max_concurrent},
                    enabled: {enabled},
                    last_probe: "{now}",
                    probe_status: "{probe_status}"
                }}
            ) {{ _docID }}
        }}"#,
        backend_id = escape_graphql_string(&args.backend_id),
        name = escape_graphql_string(&args.name),
        endpoint = escape_graphql_string(&args.endpoint),
        max_concurrent = args.max_concurrent,
        enabled = if args.enabled { "true" } else { "false" },
        probe_status = escape_graphql_string(&args.probe_status),
        now = chrono::Utc::now().to_rfc3339(),
    );
    let response = post_graphql(&args.graphql, &mutation).await?;
    let doc_id = extract_mutation_doc_id(&response, "InferenceBackend")?;
    let output = json!({
        "doc_id": doc_id,
        "backend_id": args.backend_id,
        "endpoint": args.endpoint,
        "max_concurrent": args.max_concurrent,
        "enabled": args.enabled,
        "probe_status": args.probe_status,
    });
    print_json(&output)?;
    Ok(())
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
    let created_at = chrono::Utc::now().to_rfc3339();
    let add_fields = vec![
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(&behavior_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&args.agent_did)
        )),
        optional_string_field("display_name", args.display_name.as_deref()),
        optional_string_field("system_prompt", system_prompt.as_deref()),
        optional_string_field("backend_id", args.backend_id.as_deref()),
        optional_string_field("model_name", args.model_name.as_deref()),
        optional_string_field("tool_selection_id", args.tool_selection_id.as_deref()),
        optional_string_field("inference_profile_id", args.inference_profile_id.as_deref()),
        optional_string_field("compaction_strategy", args.compaction_strategy.as_deref()),
        optional_f64_field("compaction_threshold", args.compaction_threshold),
        Some(format!(
            "enabled: {}",
            if args.enabled { "true" } else { "false" }
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
        optional_string_field("display_name", args.display_name.as_deref()),
        optional_string_field("system_prompt", system_prompt.as_deref()),
        optional_string_field("backend_id", args.backend_id.as_deref()),
        optional_string_field("model_name", args.model_name.as_deref()),
        optional_string_field("tool_selection_id", args.tool_selection_id.as_deref()),
        optional_string_field("inference_profile_id", args.inference_profile_id.as_deref()),
        optional_string_field("compaction_strategy", args.compaction_strategy.as_deref()),
        optional_f64_field("compaction_threshold", args.compaction_threshold),
        Some(format!(
            "enabled: {}",
            if args.enabled { "true" } else { "false" }
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
        behavior_id = escape_graphql_string(&behavior_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = post_graphql(&args.graphql, &mutation).await?;
    let doc_id = extract_mutation_doc_id(&response, "AgentBehavior")?;
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
    let add_fields = vec![
        Some(format!(
            r#"selection_id: "{}""#,
            escape_graphql_string(&args.selection_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&args.agent_did)
        )),
        Some(format!(
            r#"display_name: "{}""#,
            escape_graphql_string(args.display_name.as_deref().unwrap_or(""))
        )),
        Some(format!(
            "enable_file_tools: {}",
            if args.enable_file_tools {
                "true"
            } else {
                "false"
            }
        )),
        Some(format!(
            r#"file_tools_mode: "{}""#,
            escape_graphql_string(&file_tools_mode)
        )),
        Some(format!(
            "enable_bash: {}",
            if args.enable_bash { "true" } else { "false" }
        )),
        Some(format!(
            r#"bash_mode: "{}""#,
            escape_graphql_string(&bash_mode)
        )),
        string_list_field("cli_tool_names", &args.cli_tool_names),
        Some(format!(
            "enable_meta_tools: {}",
            if args.enable_meta_tools {
                "true"
            } else {
                "false"
            }
        )),
        string_list_field("delegate_to", &args.delegate_to),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let mutation = format!(
        r#"mutation {{
            upsert_ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }},
                add: {{
                    {fields}
                }},
                update: {{
                    {fields}
                }}
            ) {{ _docID }}
        }}"#,
        selection_id = escape_graphql_string(&args.selection_id),
        fields = add_fields,
    );
    let response = post_graphql(&args.graphql, &mutation).await?;
    let doc_id = extract_mutation_doc_id(&response, "ToolSelection")?;
    let output = json!({
        "doc_id": doc_id,
        "selection_id": args.selection_id,
        "agent_did": args.agent_did,
        "enable_file_tools": args.enable_file_tools,
        "file_tools_mode": file_tools_mode,
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
    let now = chrono::Utc::now().to_rfc3339();
    let next_run_at_field = nullable_string_field("next_run_at", next_run_at.as_deref());

    let add_fields = vec![
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&agent_did)
        )),
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(&behavior_id)
        )),
        Some(format!(r#"name: "{}""#, escape_graphql_string(name))),
        Some(format!(r#"prompt: "{}""#, escape_graphql_string(&prompt))),
        Some(format!("interval_secs: {}", args.interval_secs)),
        Some(format!(
            "enabled: {}",
            if args.enabled { "true" } else { "false" }
        )),
        Some(next_run_at_field.clone()),
        Some(r#"last_status: """#.to_string()),
        Some(r#"last_error: """#.to_string()),
        Some("run_count: 0".to_string()),
        Some(format!(r#"created_at: "{}""#, escape_graphql_string(&now))),
        Some(format!(r#"updated_at: "{}""#, escape_graphql_string(&now))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&agent_did)
        )),
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(&behavior_id)
        )),
        Some(format!(r#"name: "{}""#, escape_graphql_string(name))),
        Some(format!(r#"prompt: "{}""#, escape_graphql_string(&prompt))),
        Some(format!("interval_secs: {}", args.interval_secs)),
        Some(format!(
            "enabled: {}",
            if args.enabled { "true" } else { "false" }
        )),
        Some(next_run_at_field),
        Some(format!(r#"updated_at: "{}""#, escape_graphql_string(&now))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let mutation = format!(
        r#"mutation {{
            upsert_ScheduledTask(
                filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        task_id = escape_graphql_string(task_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = post_graphql(&graphql, &mutation).await?;
    let doc_id = extract_mutation_doc_id(&response, "ScheduledTask")?;
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
    let submitted = create_agent_request(
        &graphql,
        &agent_did,
        &args.content,
        args.session_id.as_deref(),
        args.behavior_id.as_deref(),
    )
    .await?;
    let request_summary = json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "agent_did": submitted.agent_did,
        "behavior_id": submitted.behavior_id,
    });
    if args.no_wait {
        print_json(&request_summary)?;
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
                admission_state
                backend_id
                execution_origin
                failure_reason
                retry_count
                max_retries
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

async fn show_runtime(args: RuntimeShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let output = load_runtime_status_output(args.home.as_deref(), &graphql, &agent_did).await?;
    print_json(&output)?;
    Ok(())
}

async fn load_runtime_status_output(
    home: Option<&Path>,
    graphql: &str,
    agent_did: &str,
) -> Result<Value> {
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
    Ok(json!({
        "home": home_dir,
        "graphql": graphql,
        "agent_did": agent_did,
        "runtime_state": runtime_state,
        "runtime": runtime_row,
    }))
}

async fn post_graphql(graphql: &str, query: &str) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let response = client
        .post(graphql)
        .json(&json!({ "query": query }))
        .send()
        .await
        .with_context(|| format!("posting GraphQL to {graphql}"))?;
    let value: serde_json::Value = response.json().await.context("decoding GraphQL JSON")?;
    if let Some(errors) = value.get("errors") {
        anyhow::bail!("graphql returned errors: {errors}");
    }
    Ok(value)
}

fn extract_mutation_doc_id(response: &Value, collection_name: &str) -> Result<String> {
    let data = response
        .get("data")
        .ok_or_else(|| anyhow::anyhow!("graphql response missing data: {response}"))?;
    for field_name in [
        format!("upsert_{collection_name}"),
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
    node: &EmbeddedNode,
    home_dir: &Path,
    args: &InitArgs,
    agent_did: &str,
) -> Result<InitSummary> {
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
    if args.tool_root.is_some() && !args.write_tools {
        anyhow::bail!("--tool-root requires --write-tools");
    }

    let endpoint = resolve_init_endpoint(args.inference_endpoint.as_deref())?;
    let backend_id = explicit_backend_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}-backend", args.agent_name));
    let backend_name = explicit_backend_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| backend_id.clone());
    let bootstrap = ensure_agent_principal(node, agent_did).await?;
    let default_behavior_id = bootstrap.default_behavior.behavior_id.clone();
    let tool_selection_id = format!("{default_behavior_id}:tools");
    let tool_ceiling = if args.write_tools {
        ToolCeilingArg::Readwrite
    } else {
        ToolCeilingArg::Readonly
    };
    let tool_root = if args.write_tools {
        Some(resolve_default_write_tool_root(args.tool_root.as_deref())?)
    } else {
        None
    };
    let templates = bootstrap_templates_for_ceiling(tool_ceiling);
    let template_vars = bootstrap_template_vars(
        home_dir,
        agent_did,
        &args.agent_name,
        &backend_id,
        &backend_name,
        &endpoint,
        model_name,
        args.max_concurrent,
        &default_behavior_id,
        &tool_selection_id,
    );
    apply_bootstrap_templates(node, templates, &template_vars).await?;

    Ok(InitSummary {
        backend_id,
        backend_name,
        endpoint,
        model_name: model_name.to_string(),
        default_behavior_id,
        tool_selection_id,
        tool_ceiling,
        tool_root: tool_root.map(|path| path.to_string_lossy().to_string()),
        created_principal: bootstrap.created_principal,
        created_default_behavior: bootstrap.created_default_behavior,
    })
}

fn resolve_init_endpoint(explicit: Option<&str>) -> Result<String> {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::env::var("INFERENCE_ENDPOINT")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| {
            anyhow::anyhow!("an inference endpoint is required; pass it to `defra-agent init` or set INFERENCE_ENDPOINT")
        })
}

fn resolve_default_write_tool_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .ok()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or_else(|| anyhow::anyhow!("unable to determine a default tool root for write tools"))
}

fn bootstrap_templates_for_ceiling(
    tool_ceiling: ToolCeilingArg,
) -> &'static [BootstrapMutationTemplate] {
    match tool_ceiling {
        ToolCeilingArg::Readonly => STANDARD_READONLY_BOOTSTRAP,
        ToolCeilingArg::Readwrite => STANDARD_READWRITE_BOOTSTRAP,
        ToolCeilingArg::MetaOnly => STANDARD_READONLY_BOOTSTRAP,
    }
}

fn bootstrap_template_vars(
    home_dir: &Path,
    agent_did: &str,
    agent_name: &str,
    backend_id: &str,
    backend_name: &str,
    endpoint: &str,
    model_name: &str,
    max_concurrent: i64,
    default_behavior_id: &str,
    tool_selection_id: &str,
) -> std::collections::BTreeMap<String, String> {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert(
        "HOME".to_string(),
        graphql_string_literal(&home_dir.to_string_lossy()),
    );
    vars.insert("AGENT_DID".to_string(), graphql_string_literal(agent_did));
    vars.insert("AGENT_NAME".to_string(), graphql_string_literal(agent_name));
    vars.insert("BACKEND_ID".to_string(), graphql_string_literal(backend_id));
    vars.insert(
        "BACKEND_NAME".to_string(),
        graphql_string_literal(backend_name),
    );
    vars.insert("ENDPOINT".to_string(), graphql_string_literal(endpoint));
    vars.insert("MODEL_NAME".to_string(), graphql_string_literal(model_name));
    vars.insert("MAX_CONCURRENT".to_string(), max_concurrent.to_string());
    vars.insert(
        "DEFAULT_BEHAVIOR_ID".to_string(),
        graphql_string_literal(default_behavior_id),
    );
    vars.insert(
        "TOOL_SELECTION_ID".to_string(),
        graphql_string_literal(tool_selection_id),
    );
    vars.insert(
        "LAST_PROBE_AT".to_string(),
        graphql_string_literal(&chrono::Utc::now().to_rfc3339()),
    );
    vars
}

async fn apply_bootstrap_templates(
    node: &EmbeddedNode,
    templates: &[BootstrapMutationTemplate],
    vars: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    for template in templates {
        let mutation =
            substitute_bootstrap_template(template.mutation, vars).with_context(|| {
                format!(
                    "rendering bootstrap template {}/{}",
                    template.collection, template.option
                )
            })?;
        let response = node.execute(&mutation).await;
        if response.has_errors() {
            anyhow::bail!(
                "bootstrap mutation failed for {}/{}: {:?}",
                template.collection,
                template.option,
                response.errors
            );
        }
    }

    Ok(())
}

fn substitute_bootstrap_template(
    template: &str,
    vars: &std::collections::BTreeMap<String, String>,
) -> Result<String> {
    let mut rendered = template.to_string();
    for (name, value) in vars {
        rendered = rendered.replace(&format!("${{{name}}}"), value);
    }
    if rendered.contains("${") {
        anyhow::bail!("unresolved bootstrap template placeholder");
    }
    Ok(rendered)
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
                status: "pending",
                lifecycle_state: "pending",
                admission_state: "released",
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
) -> Result<()> {
    let existing_tool_calls = load_existing_tool_call_keys(graphql, session_id).await?;
    let submitted =
        create_agent_request(graphql, agent_did, content, Some(session_id), behavior_id).await?;
    stream_turn_progress(
        graphql,
        &submitted,
        existing_tool_calls,
        timeout_secs,
        poll_secs,
    )
    .await?;
    Ok(())
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

fn clear_runtime_state(home_dir: &Path) -> Result<()> {
    let path = runtime_state_path(home_dir);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("removing stale runtime state {}", path.display()))?;
    }
    Ok(())
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
                "conflicting request ids provided: positional={} and --request-id={}",
                positional,
                flag
            );
        }
        (Some(request_id), _) | (_, Some(request_id)) => Ok(request_id.to_string()),
        (None, None) => anyhow::bail!("missing request id"),
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
            anyhow::bail!("timed out waiting for AgentResponse {request_id} after {timeout_secs}s of inactivity");
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
                            io::stdout().flush()?;
                        }
                    }
                }
                return Ok(response_row.unwrap_or(Value::Null));
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity",
                submitted.request_id,
                timeout_secs
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
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
