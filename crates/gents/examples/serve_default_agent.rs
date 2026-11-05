use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::defra_node::{HttpConfig, Node as EmbeddedNode};
use gents::{
    ensure_runtime_schemas, DocumentRuntimeOptions, Gents, KeyIdentity, McpPool, NodeIdentity,
    ToolCeiling,
};
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let data_dir = PathBuf::from(env_or("GENTS_DATA_DIR", "./var/defradb"));
    let http_port = env_or_u16("GENTS_HTTP_PORT", 9191);
    let node_name = env_or("GENTS_NAME", "demo");
    let key_path = std::env::var("GENTS_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("keys").join(format!("{node_name}.key")));

    let http_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), http_port);
    let identity = Arc::new(
        KeyIdentity::load_or_create(key_path, None)
            .context("creating or loading node identity key")?,
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_regolith_options(gents::store::local::storage_backend::regolith_options())
            .with_http(HttpConfig::with_addr(http_addr))
            .with_node_identity_did(identity.did())
            .build()
            .await
            .context("building embedded DefraDB node")?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;
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
        graphql = %format!("http://127.0.0.1:{http_port}/api/v0/graphql"),
        "serving default behavior"
    );

    agent.run(shutdown_rx).await
}
