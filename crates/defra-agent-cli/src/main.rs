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
    upsert_agent_behavior, AgentIdentity, BashMode, DefraAgent, DocumentRuntimeOptions,
    FileToolMode, McpPool, ProcessLifecycleObserver, ProcessLifecycleState, SimpleIdentity,
    ToolCeiling,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

const DEFAULT_AGENT_NAME: &str = "default";
const DEFAULT_HTTP_PORT: u16 = 9191;
const INIT_PRESENT_SENTINEL: &str = "__present__";
const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";

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
    #[command(name = "server", alias = "serve")]
    Server(ServeArgs),
    Chat(ChatArgs),
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
    },
    Behavior {
        #[command(subcommand)]
        command: BehaviorCommand,
    },
    ToolSelection {
        #[command(subcommand)]
        command: ToolSelectionCommand,
    },
    InferenceProfile {
        #[command(subcommand)]
        command: InferenceProfileCommand,
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
struct ServeArgs {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long, hide = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1")]
    http_addr: IpAddr,
    #[arg(long, default_value_t = DEFAULT_HTTP_PORT)]
    http_port: u16,
    #[arg(long, default_value = DEFAULT_AGENT_NAME)]
    agent_name: String,
    #[arg(long)]
    key_path: Option<PathBuf>,
    #[arg(
        long,
        num_args = 0..=1,
        value_name = "INFERENCE_ENDPOINT",
        default_missing_value = INIT_PRESENT_SENTINEL
    )]
    init: Option<String>,
    #[arg(long, hide = true)]
    inference_endpoint: Option<String>,
    #[arg(long)]
    backend_id: Option<String>,
    #[arg(long)]
    backend_name: Option<String>,
    #[arg(long)]
    model_name: Option<String>,
    #[arg(long, default_value_t = 1)]
    max_concurrent: i64,
    #[arg(long, value_enum, default_value_t = ToolCeilingArg::MetaOnly)]
    tool_ceiling: ToolCeilingArg,
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
    #[arg(long, default_value_t = 120)]
    timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    poll_secs: u64,
    #[arg(value_name = "MESSAGE")]
    message: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ToolCeilingArg {
    MetaOnly,
    Readonly,
    Readwrite,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ServeInitSummary {
    backend_id: String,
    backend_name: String,
    endpoint: String,
    model_name: String,
    default_behavior_id: String,
    created_principal: bool,
    created_default_behavior: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRuntimeState {
    home: String,
    graphql: String,
    agent_name: String,
    agent_did: String,
    default_behavior_id: String,
}

#[derive(Subcommand)]
enum BackendCommand {
    Upsert(BackendUpsertArgs),
}

#[derive(Subcommand)]
enum BehaviorCommand {
    Upsert(BehaviorUpsertArgs),
}

#[derive(Subcommand)]
enum ToolSelectionCommand {
    Upsert(ToolSelectionUpsertArgs),
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
    Upsert(InferenceProfileUpsertArgs),
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
    graphql: String,
    #[arg(long)]
    agent_did: String,
    #[arg(long)]
    content: String,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    behavior_id: Option<String>,
    #[arg(long, default_value_t = false)]
    no_wait: bool,
    #[arg(long, default_value_t = 120)]
    timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    poll_secs: u64,
}

#[derive(clap::Args)]
struct RequestShowArgs {
    #[arg(long)]
    graphql: String,
    #[arg(long)]
    request_id: String,
}

#[derive(Subcommand)]
enum ResponseCommand {
    Show(ResponseShowArgs),
    Wait(ResponseWaitArgs),
}

#[derive(clap::Args)]
struct ResponseShowArgs {
    #[arg(long)]
    graphql: String,
    #[arg(long)]
    request_id: String,
}

#[derive(clap::Args)]
struct ResponseWaitArgs {
    #[arg(long)]
    graphql: String,
    #[arg(long)]
    request_id: String,
    #[arg(long, default_value_t = 120)]
    timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    poll_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Server(args) => serve(args).await,
        Command::Chat(args) => chat(args).await,
        Command::Backend { command } => match command {
            BackendCommand::Upsert(args) => backend_upsert(args).await,
        },
        Command::Behavior { command } => match command {
            BehaviorCommand::Upsert(args) => behavior_upsert(args).await,
        },
        Command::ToolSelection { command } => match command {
            ToolSelectionCommand::Upsert(args) => tool_selection_upsert(args).await,
        },
        Command::InferenceProfile { command } => match command {
            InferenceProfileCommand::Upsert(args) => inference_profile_upsert(args).await,
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

    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let key_path = args
        .key_path
        .clone()
        .unwrap_or_else(|| default_key_path(&home_dir, &args.agent_name));
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating key directory {}", parent.display()))?;
    }
    let mut tool_ceiling = match args.tool_ceiling {
        ToolCeilingArg::MetaOnly => ToolCeiling::meta_only(),
        ToolCeilingArg::Readonly => ToolCeiling::readonly(),
        ToolCeilingArg::Readwrite => {
            let root = args.tool_root.as_ref().ok_or_else(|| {
                anyhow::anyhow!("--tool-root is required when --tool-ceiling readwrite")
            })?;
            ToolCeiling::readwrite(root)
        }
    };
    for cli_tool_arg in &args.cli_tools {
        tool_ceiling = tool_ceiling.with_cli_tool(parse_cli_tool_arg(cli_tool_arg)?);
    }
    let identity = Arc::new(SimpleIdentity::new(&args.agent_name, &key_path, None));
    let (ready_tx, mut ready_rx) = watch::channel(ProcessLifecycleState::Uninitialized);
    let init_summary =
        maybe_initialize_runtime_documents(node.as_ref(), &args, identity.did()).await?;

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
            agent_name: args.agent_name.clone(),
            agent_did: identity.did().to_string(),
            default_behavior_id: default_behavior_id.clone(),
        },
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": "serving",
            "home": home_dir,
            "agent_name": args.agent_name,
            "agent_did": identity.did(),
            "default_behavior_id": default_behavior_id,
            "runnable_behaviors": runnable_behaviors,
            "unavailable_behaviors": unavailable_behaviors,
            "init": init_summary,
            "graphql": graphql_url,
        }))?
    );

    run_handle
        .await
        .context("joining defra-agent runtime task")?
}

async fn chat(args: ChatArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let runtime_state = read_runtime_state(&home_dir)?;
    let graphql = args
        .graphql
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.graphql.clone()))
        .unwrap_or_else(|| format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"));
    let agent_name = args
        .agent_name
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_name.clone()))
        .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
    let agent_did = args
        .agent_did
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_did.clone()))
        .unwrap_or_else(|| format!("did:defra-agent:{agent_name}"));
    let session_id = args
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if !args.message.is_empty() {
        let response = submit_chat_turn(
            &graphql,
            &agent_did,
            &session_id,
            args.behavior_id.as_deref(),
            &args.message.join(" "),
            args.timeout_secs,
            args.poll_secs,
        )
        .await?;
        println!("{response}");
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

        let response = submit_chat_turn(
            &graphql,
            &agent_did,
            &session_id,
            args.behavior_id.as_deref(),
            trimmed,
            args.timeout_secs,
            args.poll_secs,
        )
        .await?;
        writeln!(stdout, "{response}")?;
    }

    Ok(())
}

async fn backend_upsert(args: BackendUpsertArgs) -> Result<()> {
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
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "doc_id": doc_id,
            "backend_id": args.backend_id,
            "endpoint": args.endpoint,
            "max_concurrent": args.max_concurrent,
            "enabled": args.enabled,
            "probe_status": args.probe_status,
        }))?
    );
    Ok(())
}

async fn behavior_upsert(args: BehaviorUpsertArgs) -> Result<()> {
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
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "doc_id": doc_id,
            "behavior_id": behavior_id,
            "agent_did": args.agent_did,
            "backend_id": args.backend_id,
            "model_name": args.model_name,
            "tool_selection_id": args.tool_selection_id,
            "inference_profile_id": args.inference_profile_id,
            "enabled": args.enabled,
        }))?
    );
    Ok(())
}

async fn tool_selection_upsert(args: ToolSelectionUpsertArgs) -> Result<()> {
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
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
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
        }))?
    );
    Ok(())
}

async fn inference_profile_upsert(args: InferenceProfileUpsertArgs) -> Result<()> {
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
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "doc_id": doc_id,
            "profile_id": args.profile_id,
            "display_name": args.display_name,
            "context_window": args.context_window,
            "max_output_tokens": args.max_output_tokens,
            "max_turns": args.max_turns,
            "temperature": args.temperature,
            "stream_batch_ms": args.stream_batch_ms,
            "deadline_duration_secs": args.deadline_duration_secs,
        }))?
    );
    Ok(())
}

async fn request_submit(args: RequestSubmitArgs) -> Result<()> {
    let submitted = create_agent_request(
        &args.graphql,
        &args.agent_did,
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
        println!("{}", serde_json::to_string_pretty(&request_summary)?);
        return Ok(());
    }

    let response = wait_for_terminal_response(
        &args.graphql,
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

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(output))?
    );
    Ok(())
}

async fn request_show(args: RequestShowArgs) -> Result<()> {
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
                retry_count
                max_retries
                created_at
                claimed_at
                deadline
            }}
        }}"#,
        request_id = escape_graphql_string(&args.request_id),
    );
    let response = post_graphql(&args.graphql, &query).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

async fn response_show(args: ResponseShowArgs) -> Result<()> {
    let query = response_query(&args.request_id);
    let response = post_graphql(&args.graphql, &query).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

async fn response_wait(args: ResponseWaitArgs) -> Result<()> {
    let response = wait_for_terminal_response(
        &args.graphql,
        &args.request_id,
        args.timeout_secs,
        args.poll_secs,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
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

async fn maybe_initialize_runtime_documents(
    node: &EmbeddedNode,
    args: &ServeArgs,
    agent_did: &str,
) -> Result<Option<ServeInitSummary>> {
    let inline_endpoint = args
        .init
        .as_deref()
        .filter(|value| *value != INIT_PRESENT_SENTINEL);
    if inline_endpoint.is_some() && args.inference_endpoint.is_some() {
        anyhow::bail!(
            "pass the inference endpoint either as --init <url> or --inference-endpoint, not both"
        );
    }
    let init_requested = args.init.is_some() || args.inference_endpoint.is_some();
    if !init_requested {
        if args.backend_id.is_some() || args.backend_name.is_some() || args.model_name.is_some() {
            anyhow::bail!("--backend-id, --backend-name, and --model-name require --init");
        }
        return Ok(None);
    }

    let endpoint = inline_endpoint
        .or(args.inference_endpoint.as_deref())
        .ok_or_else(|| anyhow::anyhow!("an inference endpoint is required when --init is set"))?;
    let bootstrap = ensure_agent_principal(node, agent_did).await?;
    let backend_id = args
        .backend_id
        .clone()
        .unwrap_or_else(|| format!("{}-backend", args.agent_name));
    let backend_name = args
        .backend_name
        .clone()
        .unwrap_or_else(|| backend_id.clone());
    let model_name = match args.model_name.clone() {
        Some(model_name) => model_name,
        None => match resolve_model_name(endpoint).await {
            Ok(model_name) => model_name,
            Err(error) => {
                tracing::warn!(
                    endpoint = %endpoint,
                    error = %error,
                    fallback_model = %defra_agent::config::DEFAULT_MODEL_NAME,
                    "could not resolve model list from inference endpoint; falling back to default model name"
                );
                defra_agent::config::DEFAULT_MODEL_NAME.to_string()
            }
        },
    };

    upsert_inference_backend_document(
        node,
        &backend_id,
        &backend_name,
        endpoint,
        &model_name,
        args.max_concurrent,
        "healthy",
    )
    .await?;

    let mut default_behavior = bootstrap.default_behavior.clone();
    default_behavior.backend_id = Some(backend_id.clone());
    default_behavior.model_name = Some(model_name.clone());
    upsert_agent_behavior(node, &default_behavior).await?;

    Ok(Some(ServeInitSummary {
        backend_id,
        backend_name,
        endpoint: endpoint.to_string(),
        model_name,
        default_behavior_id: default_behavior.behavior_id,
        created_principal: bootstrap.created_principal,
        created_default_behavior: bootstrap.created_default_behavior,
    }))
}

async fn upsert_inference_backend_document(
    node: &EmbeddedNode,
    backend_id: &str,
    name: &str,
    endpoint: &str,
    model_name: &str,
    max_concurrent: i64,
    probe_status: &str,
) -> Result<()> {
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_name = escape_graphql_string(name);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_model_name = escape_graphql_string(model_name);
    let escaped_probe_status = escape_graphql_string(probe_status);
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_name}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: {max_concurrent},
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    last_probe: "{now}",
                    probe_status: "{escaped_probe_status}"
                }},
                update: {{
                    name: "{escaped_name}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: {max_concurrent},
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    last_probe: "{now}",
                    probe_status: "{escaped_probe_status}"
                }}
            ) {{ _docID }}
        }}"#
    );

    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("upsert InferenceBackend failed: {:?}", response.errors);
    }
    Ok(())
}

async fn resolve_model_name(endpoint: &str) -> Result<String> {
    let models_url = format!("{}/models", endpoint.trim_end_matches('/'));
    let mut request = reqwest::Client::new().get(&models_url);
    if let Ok(api_key) = std::env::var("AGENT_DAEMON_API_KEY") {
        if !api_key.trim().is_empty() {
            request = request.bearer_auth(api_key);
        }
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("requesting model list from {models_url}"))?;
    let value: Value = response
        .json()
        .await
        .with_context(|| format!("decoding model list from {models_url}"))?;

    if let Some(id) = value.pointer("/data/0/id").and_then(Value::as_str) {
        return Ok(id.to_string());
    }
    if let Some(model) = value.pointer("/models/0/model").and_then(Value::as_str) {
        return Ok(model.to_string());
    }

    anyhow::bail!("could not resolve a model id from {models_url}: {value}");
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
                status
                content
                token_count
                progress_seq
                completed_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

#[derive(Debug, Clone)]
struct SubmittedRequest {
    request_id: String,
    session_id: String,
    agent_did: String,
    behavior_id: Option<String>,
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
) -> Result<String> {
    let submitted =
        create_agent_request(graphql, agent_did, content, Some(session_id), behavior_id).await?;
    let response =
        wait_for_terminal_response(graphql, &submitted.request_id, timeout_secs, poll_secs).await?;
    Ok(response
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
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

fn runtime_state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_STATE_FILE_NAME)
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

fn default_key_path(home_dir: &Path, agent_name: &str) -> PathBuf {
    home_dir.join("keys").join(format!("{agent_name}.key"))
}

fn display_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(addr) if addr == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
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

async fn wait_for_terminal_response(
    graphql: &str,
    request_id: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let query = response_query(request_id);
        let response = post_graphql(graphql, &query).await?;
        let rows = response
            .pointer("/data/AgentResponse")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if let Some(row) = rows.first() {
            let status = row
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if matches!(status, "complete" | "error") {
                return Ok(row.clone());
            }
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for AgentResponse {request_id}");
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}
