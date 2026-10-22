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
    pub node_name: String,
    pub tool_ceiling: ManagedToolCeiling,
    pub tool_root: Option<PathBuf>,
    pub store_key_custody: gents::store_key::StoreKeyCustodyChoice,
}

/// Provisioning drives `init`, whose node opening is not `Send`, on a blocking
/// thread of `runtime`; that runtime's threads must have the large stacks the
/// provisioning depth needs.
pub async fn ensure_standard_home(
    runtime: &tokio::runtime::Handle,
    options: ProvisionOptions,
) -> Result<()> {
    let blocking = runtime.clone();
    runtime
        .spawn_blocking(move || blocking.block_on(ensure_standard_home_inner(options)))
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
        "--node-name".to_string(),
        options.node_name,
        "--tool-package".to_string(),
        tool_package.to_string(),
        "--engineer".to_string(),
        "--inference-url".to_string(),
        crate::DEFAULT_INIT_ENDPOINT.to_string(),
    ];
    if let Some(tool_root) = options.tool_root {
        argv.push("--tool-root".to_string());
        argv.push(tool_root.display().to_string());
    }
    if options.store_key_custody == gents::store_key::StoreKeyCustodyChoice::File {
        argv.extend(["--store-key-custody".to_string(), "file".to_string()]);
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
