use std::path::PathBuf;

use anyhow::Result;
use gents::runtime::embedded::EmbeddedHome;
use gents::{DocumentRuntimeOptions, Gents};
use gents_tools::{mcp::pool::McpPool, surface::ToolCeiling};
use tokio::sync::watch;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let home_dir = PathBuf::from(env_or("GENTS_HOME", "./var/gents-home"));
    let node_name = env_or("GENTS_NAME", "demo");
    let home = if home_dir.join("node.key").exists() {
        EmbeddedHome::open_retained(&home_dir).await?
    } else {
        EmbeddedHome::create_retained(&home_dir).await?
    };
    let node = home.node.clone();
    let identity = home.identity.clone();
    let graphql = home.graphql_endpoint().unwrap_or_default().to_owned();
    let agent = Gents::from_default_agent_documents(
        node,
        identity.clone(),
        DocumentRuntimeOptions {
            mcp_pool: McpPool::new(),
            local_hostname: Some("localhost".to_string()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });

    tracing::info!(
        node_name,
        node_did = agent.node_did(),
        graphql = %graphql,
        "serving default behavior"
    );

    agent.run(shutdown_rx).await
}
