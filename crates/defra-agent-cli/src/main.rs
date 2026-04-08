use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{ensure_runtime_schemas, DefraAgent, McpPool, SimpleIdentity, ToolSet};
use serde_json::json;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "defra-agent-cli", about = "Consumer CLI for the defra-agent library")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
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
    data_dir: PathBuf,
    #[arg(long, default_value = "127.0.0.1")]
    http_addr: IpAddr,
    #[arg(long, default_value_t = 9191)]
    http_port: u16,
    #[arg(long)]
    agent_name: String,
    #[arg(long)]
    backend_id: String,
    #[arg(long)]
    model_endpoint: String,
    #[arg(long, default_value = "default")]
    model_name: String,
    #[arg(long)]
    key_path: Option<PathBuf>,
    #[arg(long)]
    system_prompt_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = ToolMode::Readonly)]
    tool_mode: ToolMode,
    #[arg(long)]
    tool_root: Option<PathBuf>,
    #[arg(long, default_value_t = 131_072)]
    context_window: usize,
    #[arg(long, default_value_t = 32_768)]
    max_output_tokens: usize,
    #[arg(long, default_value_t = 50)]
    max_turns: usize,
    #[arg(long, default_value_t = 1_000)]
    stream_batch_ms: u64,
    #[arg(long, default_value_t = 900)]
    deadline_secs: u64,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ToolMode {
    Readonly,
    Readwrite,
}

#[derive(Subcommand)]
enum BackendCommand {
    Upsert(BackendUpsertArgs),
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
        Command::Serve(args) => serve(args).await,
        Command::Backend { command } => match command {
            BackendCommand::Upsert(args) => backend_upsert(args).await,
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
    let backend_id = args.backend_id.clone();
    let http_addr = SocketAddr::new(args.http_addr, args.http_port);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&args.data_dir)
            .with_http(defra_node::HttpConfig::with_addr(http_addr))
            .build()
            .await
            .context("building embedded defra node")?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;

    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let system_prompt = match args.system_prompt_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("reading system prompt from {}", path.display()))?,
        None => String::new(),
    };
    let key_path = args
        .key_path
        .unwrap_or_else(|| default_key_path(&args.data_dir, &args.agent_name));
    let native_tools = match args.tool_mode {
        ToolMode::Readonly => ToolSet::readonly(),
        ToolMode::Readwrite => {
            let root = args.tool_root.as_ref().ok_or_else(|| {
                anyhow::anyhow!("--tool-root is required when --tool-mode readwrite")
            })?;
            ToolSet::readwrite(root)
        }
    };

    let agent = DefraAgent::builder()
        .node(node)
        .mcp_pool(McpPool::new())
        .local_hostname(local_hostname)
        .profile(args.agent_name.clone())
        .identity(SimpleIdentity::new(&args.agent_name, &key_path, None))
        .system_prompt(system_prompt)
        .native_tools(native_tools)
        .model_endpoint(args.model_endpoint)
        .model_name(args.model_name)
        .context_window(args.context_window)
        .max_output_tokens(args.max_output_tokens)
        .max_turns(args.max_turns)
        .stream_batch_ms(args.stream_batch_ms)
        .deadline_duration(Duration::from_secs(args.deadline_secs))
        .backend_id(backend_id.clone())
        .done()
        .build()?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": "serving",
            "agent_name": args.agent_name,
            "agent_did": format!("did:defra-agent:{}", args.agent_name),
            "graphql": format!("http://{}:{}/api/v0/graphql", display_host(args.http_addr), args.http_port),
            "backend_id": backend_id,
        }))?
    );

    agent.run(shutdown_rx).await
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
    post_graphql(&args.graphql, &mutation).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "backend_id": args.backend_id,
            "endpoint": args.endpoint,
            "max_concurrent": args.max_concurrent,
            "enabled": args.enabled,
            "probe_status": args.probe_status,
        }))?
    );
    Ok(())
}

async fn request_submit(args: RequestSubmitArgs) -> Result<()> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = args
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
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
        agent_did = escape_graphql_string(&args.agent_did),
        session_id = escape_graphql_string(&session_id),
        content = escape_graphql_string(&args.content),
    );
    post_graphql(&args.graphql, &mutation).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "request_id": request_id,
            "session_id": session_id,
            "agent_did": args.agent_did,
        }))?
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        let query = response_query(&args.request_id);
        let response = post_graphql(&args.graphql, &query).await?;
        let rows = response
            .pointer("/data/AgentResponse")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if let Some(row) = rows.first() {
            let status = row.get("status").and_then(|value| value.as_str()).unwrap_or("");
            if matches!(status, "complete" | "error") {
                println!("{}", serde_json::to_string_pretty(row)?);
                return Ok(());
            }
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for AgentResponse {}", args.request_id);
        }

        tokio::time::sleep(Duration::from_secs(args.poll_secs)).await;
    }
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

fn response_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
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

fn default_key_path(data_dir: &Path, agent_name: &str) -> PathBuf {
    data_dir.join("keys").join(format!("{agent_name}.key"))
}

fn display_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(addr) if addr == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
}
