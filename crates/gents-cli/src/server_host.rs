//! Reusable, UI-agnostic host for an in-process Gents server.
//!
//! The CLI owns argument parsing, signals, and presentation. Embedders own the
//! returned handle and therefore the server lifetime.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use gents_protocol::enrollment::{
    EnrollmentOperatorAction, DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use crate::cli::{Cli, Command};

pub use crate::cli::args::ToolCeilingArg as ManagedToolCeiling;

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

    pub fn status_url(&self) -> String {
        format!("http://{}:{}/status", self.http_addr, self.http_port)
    }
}

#[derive(Debug, Clone)]
pub struct ProvisionOptions {
    pub home: PathBuf,
    pub agent_name: String,
    pub tool_ceiling: ManagedToolCeiling,
    pub tool_root: Option<PathBuf>,
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
    pub tool_ceiling: ManagedToolCeiling,
    pub tool_root: Option<String>,
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
    if let Some(mut stored) = crate::read_init_config(&options.home)? {
        stored.tool_ceiling = options.tool_ceiling;
        stored.tool_root = options
            .tool_root
            .map(|path| path.to_string_lossy().into_owned());
        crate::write_init_config(&options.home, &stored)?;
        return Ok(());
    }

    let tool_package = match options.tool_ceiling {
        ManagedToolCeiling::MetaOnly => "minimal",
        ManagedToolCeiling::Readonly => "readonly",
        ManagedToolCeiling::Readwrite => "yolo",
    };

    let mut argv = vec![
        "gents".to_string(),
        "init".to_string(),
        "--home".to_string(),
        options.home.display().to_string(),
        "--agent-name".to_string(),
        options.agent_name,
        "--tool-package".to_string(),
        tool_package.to_string(),
        "--setup-steward".to_string(),
        "--inference-url".to_string(),
        crate::DEFAULT_INIT_ENDPOINT.to_string(),
    ];
    if let Some(tool_root) = options.tool_root {
        argv.push("--tool-root".to_string());
        argv.push(tool_root.display().to_string());
    }
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

/// Approve the desktop client enrollment using the identity owned by a
/// co-hosted managed runtime. The signed durable enrollment documents remain
/// the route authority; this helper only drives the existing operator API.
pub async fn approve_managed_client_enrollment(
    home: &Path,
    graphql: &str,
    request_id: &str,
) -> Result<()> {
    let request_id = request_id.trim();
    anyhow::ensure!(
        !request_id.is_empty(),
        "managed enrollment request id is empty"
    );
    crate::commands::p2p::enrollment_admin::submit_enrollment_decision(
        home,
        graphql,
        request_id,
        EnrollmentOperatorAction::Approve,
        DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
    )
    .await
    .context("approving desktop enrollment on managed runtime")?;
    Ok(())
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
        tool_ceiling: match required_string(output, "tool_ceiling")?.as_str() {
            "meta-only" => ManagedToolCeiling::MetaOnly,
            "readonly" => ManagedToolCeiling::Readonly,
            "readwrite" => ManagedToolCeiling::Readwrite,
            value => {
                anyhow::bail!("managed server readiness returned unknown tool_ceiling {value}")
            }
        },
        tool_root: output
            .get("tool_root")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
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
            "p2p_listen_addresses": ["iroh://peer-local"],
            "tool_ceiling": "readwrite",
            "tool_root": "/Users/test"
        }))
        .unwrap();
        assert_eq!(ready.agent_did, "did:key:zLocal");
        assert_eq!(ready.p2p_peer_id.as_deref(), Some("peer-local"));
        assert_eq!(ready.p2p_listen_addresses, ["iroh://peer-local"]);
        assert_eq!(ready.tool_ceiling, ManagedToolCeiling::Readwrite);
        assert_eq!(ready.tool_root.as_deref(), Some("/Users/test"));
    }

    #[tokio::test]
    async fn reprovisioning_changes_only_the_existing_homes_process_ceiling() {
        let temp = tempfile::tempdir().unwrap();
        let first_root = temp.path().join("first root");
        let second_root = temp.path().join("second root");
        std::fs::create_dir(&first_root).unwrap();
        std::fs::create_dir(&second_root).unwrap();
        crate::write_init_config(
            temp.path(),
            &crate::shared::StoredInitConfig {
                home: temp.path().display().to_string(),
                agent_name: "Forge".to_string(),
                agent_did: "did:key:zPreserved".to_string(),
                key_path: Some(temp.path().join("agent.key").display().to_string()),
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: Some(crate::cli::args::ToolPackageArg::Yolo),
                tool_ceiling: ManagedToolCeiling::Readwrite,
                tool_root: Some(first_root.display().to_string()),
            },
        )
        .unwrap();

        ensure_standard_home_inner(ProvisionOptions {
            home: temp.path().to_path_buf(),
            agent_name: "A different ignored name".to_string(),
            tool_ceiling: ManagedToolCeiling::Readonly,
            tool_root: Some(second_root.clone()),
        })
        .await
        .unwrap();

        let stored = crate::read_init_config(temp.path()).unwrap().unwrap();
        assert_eq!(stored.agent_name, "Forge");
        assert_eq!(stored.agent_did, "did:key:zPreserved");
        assert_eq!(stored.tool_ceiling, ManagedToolCeiling::Readonly);
        assert_eq!(stored.tool_root.as_deref(), second_root.to_str());
    }
}
