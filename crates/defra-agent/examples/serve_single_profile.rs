use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_agent::defra_node::{EmbeddedNode, HttpConfig};
use defra_agent::{ensure_runtime_schemas, DefraAgent, McpPool, SimpleIdentity, ToolSet};
use tokio::sync::watch;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_or_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_or_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_dir = PathBuf::from(env_or("DEFRA_AGENT_DATA_DIR", "./var/defradb"));
    let http_port = env_or_u16("DEFRA_AGENT_HTTP_PORT", 9191);
    let agent_name = env_or("DEFRA_AGENT_NAME", "demo");
    let backend_id = env_or("DEFRA_AGENT_BACKEND_ID", "demo-backend");
    let model_endpoint = env_or("DEFRA_AGENT_MODEL_ENDPOINT", "http://127.0.0.1:8000/v1");
    let model_name = env_or("DEFRA_AGENT_MODEL_NAME", "default");
    let system_prompt = std::env::var("DEFRA_AGENT_SYSTEM_PROMPT").unwrap_or_default();
    let deadline_secs = env_or_u64("DEFRA_AGENT_DEADLINE_SECS", 900);
    let key_path = std::env::var("DEFRA_AGENT_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("keys").join(format!("{agent_name}.key")));

    let http_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), http_port);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_http(HttpConfig::with_addr(http_addr))
            .build()
            .await
            .context("building embedded defra node")?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;

    let agent = DefraAgent::builder()
        .node(node)
        .mcp_pool(McpPool::new())
        .local_hostname("localhost")
        .profile(agent_name.clone())
        .identity(SimpleIdentity::new(&agent_name, key_path, None))
        .system_prompt(system_prompt)
        .native_tools(ToolSet::readonly())
        .model_endpoint(model_endpoint)
        .model_name(model_name)
        .deadline_duration(Duration::from_secs(deadline_secs))
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
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "serving",
            "agent_name": agent_name,
            "agent_did": format!("did:defra-agent:{agent_name}"),
            "graphql": format!("http://127.0.0.1:{http_port}/api/v0/graphql"),
            "backend_id": backend_id,
        }))?
    );

    agent.run(shutdown_rx).await
}
