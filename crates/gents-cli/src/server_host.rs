//! Shared provisioning and operator helpers for the Gents server.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use gents_protocol::enrollment::{
    EnrollmentOperatorAction, DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
};

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

pub fn initialized_home(path: &Path) -> bool {
    path.join(crate::INIT_CONFIG_FILE_NAME).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_config_matches_cli_server_defaults() {
        let config = ServerConfig::standard(PathBuf::from("/tmp/gents-test"));
        assert_eq!(config.http_addr, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(config.http_port, crate::DEFAULT_HTTP_PORT);
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
