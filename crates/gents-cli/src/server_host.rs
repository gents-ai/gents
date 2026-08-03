//! Reusable, UI-agnostic host for an in-process Gents server.
//!
//! The CLI owns argument parsing, signals, and presentation. Embedders own the
//! returned handle and therefore the server lifetime.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use crate::cli::{Cli, Command};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub home: PathBuf,
    pub http_addr: IpAddr,
    pub http_port: u16,
}

impl ServerConfig {
    pub fn standard(home: PathBuf) -> Self {
        Self {
            home,
            http_addr: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            http_port: crate::DEFAULT_HTTP_PORT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProvisionOptions {
    pub home: PathBuf,
    pub agent_name: String,
    pub tool_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerReady {
    pub agent_name: String,
    pub agent_did: String,
    pub graphql: String,
    pub p2p_transport: String,
    pub p2p_peer_id: Option<String>,
    pub p2p_listen_addresses: Vec<String>,
}

pub struct RunningServer {
    ready: ServerReady,
    shutdown_tx: watch::Sender<bool>,
    thread: std::thread::JoinHandle<Result<()>>,
}

impl RunningServer {
    pub fn ready(&self) -> &ServerReady {
        &self.ready
    }

    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        join_server_thread(self.thread).await
    }

    pub async fn wait(self) -> Result<()> {
        join_server_thread(self.thread).await
    }
}

pub async fn ensure_standard_home(options: ProvisionOptions) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        std::thread::Builder::new()
            .name("gents-managed-provision".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_stack_size(16 * 1024 * 1024)
                    .build()
                    .context("building managed Gents provision runtime")?
                    .block_on(ensure_standard_home_inner(options))
            })
            .context("spawning managed Gents provision thread")?
            .join()
            .map_err(|_| anyhow::anyhow!("managed Gents provision thread panicked"))?
    })
    .await
    .context("joining managed Gents provision task")?
}

async fn ensure_standard_home_inner(options: ProvisionOptions) -> Result<()> {
    if options.home.join(crate::INIT_CONFIG_FILE_NAME).is_file() {
        return Ok(());
    }

    let argv = vec![
        "gents".to_string(),
        "init".to_string(),
        "--home".to_string(),
        options.home.display().to_string(),
        "--agent-name".to_string(),
        options.agent_name,
        "--tool-package".to_string(),
        "readonly".to_string(),
        "--tool-root".to_string(),
        options.tool_root.display().to_string(),
        "--inference-url".to_string(),
        crate::DEFAULT_INIT_ENDPOINT.to_string(),
        "--model-name".to_string(),
        crate::DEFAULT_INIT_MODEL_NAME.to_string(),
    ];
    let cli = Cli::try_parse_from(argv).context("building standard Gents provision request")?;
    let Command::Init(args) = cli.command else {
        unreachable!("standard provision argv must parse as init")
    };
    crate::commands::init::init(args).await
}

pub async fn start_server(config: ServerConfig) -> Result<RunningServer> {
    let argv = vec![
        "gents".to_string(),
        "server".to_string(),
        "--home".to_string(),
        config.home.display().to_string(),
        "--http-addr".to_string(),
        config.http_addr.to_string(),
        "--http-port".to_string(),
        config.http_port.to_string(),
    ];
    let cli = Cli::try_parse_from(argv).context("building managed Gents server request")?;
    let Command::Server(args) = cli.command else {
        unreachable!("managed server argv must parse as server")
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("gents-managed-server".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(16 * 1024 * 1024)
                .build()
                .context("building managed Gents server runtime")?
                .block_on(crate::commands::serve::serve_with_control(
                    args,
                    Some(shutdown_rx),
                    Some(ready_tx),
                ))
        })
        .context("spawning managed Gents server thread")?;

    let output = match ready_rx.await {
        Ok(output) => output,
        Err(_) => {
            return match join_server_thread(thread).await {
                Err(error) => Err(error).context("managed Gents server exited before readiness"),
                Ok(()) => anyhow::bail!("managed Gents server exited before readiness"),
            };
        }
    };
    let ready = ready_from_output(&output)?;
    Ok(RunningServer {
        ready,
        shutdown_tx,
        thread,
    })
}

async fn join_server_thread(thread: std::thread::JoinHandle<Result<()>>) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("managed Gents server thread panicked"))?
    })
    .await
    .context("joining managed Gents server join task")?
}

fn ready_from_output(output: &Value) -> Result<ServerReady> {
    Ok(ServerReady {
        agent_name: required_string(output, "agent_name")?,
        agent_did: required_string(output, "agent_did")?,
        graphql: required_string(output, "graphql")?,
        p2p_transport: required_string(output, "p2p_transport")?,
        p2p_peer_id: output
            .get("p2p_peer_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        p2p_listen_addresses: output
            .get("p2p_listen_addresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("managed server readiness omitted {field}"))
}

pub fn initialized_home(path: &Path) -> bool {
    path.join(crate::INIT_CONFIG_FILE_NAME).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn standard_config_matches_cli_server_defaults() {
        let config = ServerConfig::standard(PathBuf::from("/tmp/gents-test"));
        assert_eq!(config.http_addr, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(config.http_port, crate::DEFAULT_HTTP_PORT);
    }

    #[test]
    fn readiness_projection_preserves_runtime_identity_and_p2p() {
        let ready = ready_from_output(&json!({
            "agent_name": "local",
            "agent_did": "did:key:zLocal",
            "graphql": "http://127.0.0.1:9191/api/v0/graphql",
            "p2p_transport": "iroh",
            "p2p_peer_id": "peer-local",
            "p2p_listen_addresses": ["iroh://peer-local"]
        }))
        .unwrap();
        assert_eq!(ready.agent_did, "did:key:zLocal");
        assert_eq!(ready.p2p_peer_id.as_deref(), Some("peer-local"));
        assert_eq!(ready.p2p_listen_addresses, ["iroh://peer-local"]);
    }
}
