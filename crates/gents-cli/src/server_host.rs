use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use gents_protocol::enrollment::{
    EnrollmentOperatorAction, EnrollmentOperatorQuery,
    DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
};

use crate::cli::{Cli, Command};

pub use crate::cli::args::ToolCeilingArg as ManagedToolCeiling;
pub use crate::commands::p2p::enrollment_admin::EnrollmentDecisionRejected;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedPendingEnrollment {
    pub request_id: String,
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub owner_agent: String,
    pub expires_at: String,
    pub profile: Option<String>,
    pub issued_at: Option<String>,
}

#[derive(serde::Deserialize)]
struct ManagedPendingEnrollments {
    pending: Vec<ManagedPendingEnrollment>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedEnrollmentMember {
    pub request_id: String,
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub profile: String,
    pub authorization_expires_at: String,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedEnrollmentHistoryState {
    Approved,
    Denied,
    Revoked,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedEnrollmentHistoryEntry {
    pub request_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub profile: String,
    pub state: ManagedEnrollmentHistoryState,
    pub decided_at: String,
    pub decided_by: String,
    pub revoked_at: Option<String>,
    pub revoked_by: Option<String>,
}

#[derive(serde::Deserialize)]
struct ManagedEnrollmentMembers {
    members: Vec<ManagedEnrollmentMember>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedEnrollmentHistory {
    pub history: Vec<ManagedEnrollmentHistoryEntry>,
    pub observed_count: usize,
    pub limit: usize,
    pub omits_superseded: bool,
    pub conflicts_omitted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedEnrollmentDecision {
    Approve { lease_seconds: u64 },
    Deny,
    Revoke,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct ManagedEnrollmentDecisionOutcome {
    pub request_id: String,
    pub state: String,
    pub decision_doc_id: String,
    pub revision_doc_id: Option<String>,
    pub delivery_pending: bool,
}

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
    pub store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice,
}

/// Provisioning drives `init`, whose node opening is not `Send`, on a blocking
/// thread of `runtime`; that runtime's threads must have the large stacks the
/// provisioning depth needs.
pub async fn ensure_standard_home(
    runtime: &tokio::runtime::Handle,
    options: ProvisionOptions,
) -> Result<()> {
    gents::store::local::storage_backend::drive_unsendable(runtime, move || {
        ensure_standard_home_inner(options)
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
    if options.store_key_custody == gents::store::local::store_key::StoreKeyCustodyChoice::File {
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
    decide_managed_enrollment(
        home,
        graphql,
        request_id,
        ManagedEnrollmentDecision::Approve {
            lease_seconds: DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
        },
    )
    .await
    .context("approving desktop enrollment on managed runtime")?;
    Ok(())
}

pub async fn pending_managed_enrollments(
    home: &Path,
    graphql: &str,
) -> Result<Vec<ManagedPendingEnrollment>> {
    let body =
        crate::commands::p2p::enrollment_admin::load_pending_enrollments(home, graphql).await?;
    let result: ManagedPendingEnrollments =
        serde_json::from_value(body).context("decoding managed pending enrollments")?;
    Ok(result.pending)
}

pub async fn managed_enrollment_members(
    home: &Path,
    graphql: &str,
) -> Result<Vec<ManagedEnrollmentMember>> {
    let body = crate::commands::p2p::enrollment_admin::load_enrollment_query(
        home,
        graphql,
        EnrollmentOperatorQuery::Members,
    )
    .await?;
    let result: ManagedEnrollmentMembers =
        serde_json::from_value(body).context("decoding managed enrollment members")?;
    Ok(result.members)
}

pub async fn managed_enrollment_history(
    home: &Path,
    graphql: &str,
) -> Result<ManagedEnrollmentHistory> {
    let body = crate::commands::p2p::enrollment_admin::load_enrollment_query(
        home,
        graphql,
        EnrollmentOperatorQuery::History,
    )
    .await?;
    let result: ManagedEnrollmentHistory =
        serde_json::from_value(body).context("decoding managed enrollment history")?;
    Ok(result)
}

pub async fn decide_managed_enrollment(
    home: &Path,
    graphql: &str,
    request_id: &str,
    decision: ManagedEnrollmentDecision,
) -> Result<ManagedEnrollmentDecisionOutcome> {
    let (action, lease_seconds) = match decision {
        ManagedEnrollmentDecision::Approve { lease_seconds } => {
            (EnrollmentOperatorAction::Approve, lease_seconds)
        }
        ManagedEnrollmentDecision::Deny => (EnrollmentOperatorAction::Deny, 0),
        ManagedEnrollmentDecision::Revoke => (EnrollmentOperatorAction::Revoke, 0),
    };
    let body = crate::commands::p2p::enrollment_admin::submit_enrollment_decision(
        home,
        graphql,
        request_id,
        action,
        lease_seconds,
    )
    .await?;
    serde_json::from_value(body).context("decoding managed enrollment decision")
}

pub fn initialized_home(path: &Path) -> bool {
    path.join(crate::INIT_CONFIG_FILE_NAME).is_file()
}
