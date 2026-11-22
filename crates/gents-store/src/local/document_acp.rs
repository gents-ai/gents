use std::path::{Path, PathBuf};
use std::sync::Arc;

use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use serde::{Deserialize, Serialize};

use crate::identity::NodeIdentity;
use crate::local::home::{StoredInitConfig, SECURE_ENCLAVE_IDENTITY_BACKEND};
use crate::local::store_key::StoreKey;

#[cfg(feature = "vera")]
mod embedded_vera;
mod enrollment;
mod http_edge;
#[cfg(test)]
mod tests;

pub use enrollment::{
    enrollment, enrollment_root, ensure_enrollment, EnrollmentAnswer, EnrollmentRequest,
    EnrollmentRoot,
};
pub use http_edge::HttpEdge;

const VERA_DIR_NAME: &str = "vera";
const NODE_DIR_NAME: &str = "node";
const DEPLOYMENT_RECORD: &str = ".gents-embedded-vera-deployment";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Deployment {
    consensus_key: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentAcp {
    #[default]
    Local,
    Vera,
}

impl DocumentAcp {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

impl std::fmt::Display for DocumentAcp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Local => "local",
            Self::Vera => "vera",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentAcpError {
    #[error("this home cannot be changed to work differently; set up a new home instead")]
    Switch {
        recorded: DocumentAcp,
        requested: DocumentAcp,
    },
    #[error("this version of Gents cannot open this home; update Gents on this device")]
    Unavailable,
    #[error(
        "this home cannot be kept with a Secure Enclave device key; set it up with a file or \
         keychain key instead"
    )]
    NodeKeyCannotSign { node: String },
    #[error("make this change from the Gents app or CLI on your own device")]
    HeldUserKey,
    #[error(
        "this version of Gents cannot connect to this home; update Gents on this device and try \
         again"
    )]
    DelegationRequired,
    /// The provider grants no transport relationships yet (defradb.rs#2980),
    /// so the runtime publishes no peer records as its Node.
    #[error(
        "peer networking is not available on this home yet, so it cannot be shared with other \
         devices"
    )]
    TransportGrantsAwaitProvider,
    #[error(
        "this home was created by another version of Gents or with another device key; open it \
         with the Gents version and key that created it"
    )]
    DeploymentDiffers { what: &'static str },
    #[error(
        "part of this home is missing ({}); nothing was created in its place. Restore the home \
         from a backup",
        path.display()
    )]
    DeploymentMissing { path: PathBuf },
    #[error(
        "part of this home ({}) belongs to another home; restore it from a backup of this home",
        path.display()
    )]
    DeploymentReplaced { path: PathBuf },
    #[error("reading this home's record {}", path.display())]
    Record {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("inspecting {}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl DocumentAcpError {
    pub fn in_chain<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        gents_protocol::error_chain::find_source::<Self>(error)
    }

    pub(crate) fn refusal(&self) -> Option<IncompatibleStoreKind> {
        match self {
            Self::DeploymentDiffers { .. } => Some(IncompatibleStoreKind::OtherVersionSchema),
            Self::DeploymentMissing { .. }
            | Self::DeploymentReplaced { .. }
            | Self::Record { .. } => Some(IncompatibleStoreKind::UnsupportedOrCorrupt),
            Self::Switch { .. }
            | Self::Unavailable
            | Self::NodeKeyCannotSign { .. }
            | Self::HeldUserKey
            | Self::DelegationRequired
            | Self::TransportGrantsAwaitProvider
            | Self::Inspect { .. } => None,
        }
    }
}

type Result<T> = std::result::Result<T, DocumentAcpError>;

/// What a runtime's public status reports of its delegation audience
/// (`Identity.UserDelegation.statusReports`).
pub fn status_reports(
    provider: DocumentAcp,
    audience: Option<DelegationAudience>,
) -> ReportedDelegationAudience {
    match (provider, audience) {
        (DocumentAcp::Local, _) => ReportedDelegationAudience::Absent,
        (DocumentAcp::Vera, Some(audience)) => ReportedDelegationAudience::Named(audience),
        (DocumentAcp::Vera, None) => ReportedDelegationAudience::Unavailable,
    }
}

/// Whether the runtime may sign a pack's effects with a User key kept in its
/// home (`Identity.UserDelegation.runtimeSignsWithHeldUserKey`).
pub fn runtime_signs_with_held_user_key(provider: DocumentAcp) -> Result<()> {
    match provider {
        DocumentAcp::Local => Ok(()),
        DocumentAcp::Vera => Err(DocumentAcpError::HeldUserKey),
    }
}

/// The provider's time `node`'s Vera access control reports, in Unix
/// seconds, which a client mints its delegations at
/// (`Identity.UserDelegation.issueAt`); none for a node that decides access in
/// its own store or whose provider has verified no block.
pub fn node_provider_time(node: &defra_node::Node) -> Option<u64> {
    #[cfg(feature = "vera")]
    {
        embedded_vera::provider_time(node)
    }
    #[cfg(not(feature = "vera"))]
    {
        let _ = node;
        None
    }
}

/// The worker and deployment `node`'s Vera access control acts under a
/// delegation for; none for a node that decides access in its own store.
pub fn node_delegation_audience(node: &defra_node::Node) -> Option<DelegationAudience> {
    #[cfg(feature = "vera")]
    {
        embedded_vera::delegation_audience(node)
    }
    #[cfg(not(feature = "vera"))]
    {
        let _ = node;
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    Proceeds,
    AwaitsTransportGrants,
}

/// Whether the runtime publishes peer records as its Node
/// (`Identity.NodePublication.publication`).
pub fn publication(provider: DocumentAcp) -> Publication {
    match provider {
        DocumentAcp::Local => Publication::Proceeds,
        DocumentAcp::Vera => Publication::AwaitsTransportGrants,
    }
}

/// Refuses a Node-run peer publication on `node` before anything is written
/// while its provider grants no transport relationships.
pub fn ensure_node_publication(node: &defra_node::Node) -> Result<()> {
    match publication(provider_of(node)) {
        Publication::Proceeds => Ok(()),
        Publication::AwaitsTransportGrants => Err(DocumentAcpError::TransportGrantsAwaitProvider),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationGrantRoute {
    Direct,
    OwnerStaged,
}

/// How a configuration actor changes a relation only the document's owner
/// manages (`Identity.OwnerStaged.configurationGrantRoute`).
pub fn configuration_grant_route(
    provider: DocumentAcp,
    actor_owns: bool,
) -> ConfigurationGrantRoute {
    match (provider, actor_owns) {
        (DocumentAcp::Local, _) | (DocumentAcp::Vera, true) => ConfigurationGrantRoute::Direct,
        (DocumentAcp::Vera, false) => ConfigurationGrantRoute::OwnerStaged,
    }
}

/// The runtime's periodic writes of its own peer records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Periodic {
    RegistryHeartbeat,
    EndpointHeartbeat,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodicLoop {
    Beats,
    Idle,
}

/// What a periodic peer publication's loop does on a home whose document
/// access control runs on `provider` (`Identity.NodePublication.periodic`).
pub fn periodic(provider: DocumentAcp, _: Periodic) -> PeriodicLoop {
    match publication(provider) {
        Publication::Proceeds => PeriodicLoop::Beats,
        Publication::AwaitsTransportGrants => PeriodicLoop::Idle,
    }
}

/// Whether a watcher on a home whose document access control runs on
/// `provider` also waits for access changes (`AccessWake.listens`).
pub fn access_wakes(provider: DocumentAcp) -> bool {
    match provider {
        DocumentAcp::Local => false,
        DocumentAcp::Vera => true,
    }
}

/// [`periodic`] for `node`, whose provider is fixed while it runs.
pub fn node_periodic(node: &defra_node::Node, publication: Periodic) -> PeriodicLoop {
    periodic(provider_of(node), publication)
}

/// `DocumentAcpChoice.PolicyIds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyIds {
    Counter,
    Seeded,
}

/// `DocumentAcpChoice.policyIds`.
pub fn policy_ids(provider: DocumentAcp) -> PolicyIds {
    match provider {
        DocumentAcp::Local => PolicyIds::Counter,
        DocumentAcp::Vera => PolicyIds::Seeded,
    }
}

/// The IDs a home on `provider` names its built-in policies by.
pub fn home_policy_ids(provider: DocumentAcp) -> &'static gents_protocol::schemas::HomePolicyIds {
    match policy_ids(provider) {
        PolicyIds::Counter => gents_protocol::schemas::HomePolicyIds::node_store(),
        PolicyIds::Seeded => gents_protocol::schemas::HomePolicyIds::embedded_vera(),
    }
}

/// [`home_policy_ids`] of the home `node` serves.
pub fn node_policy_ids(node: &defra_node::Node) -> &'static gents_protocol::schemas::HomePolicyIds {
    home_policy_ids(provider_of(node))
}

/// Where `node` decides document access.
pub fn provider_of(node: &defra_node::Node) -> DocumentAcp {
    #[cfg(any(test, feature = "vera"))]
    {
        if node.vera_acp().is_some() {
            return DocumentAcp::Vera;
        }
    }
    let _ = node;
    DocumentAcp::Local
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    IdentityToken,
    Delegation,
}

/// What the runtime does with an authenticated request at its HTTP edge
/// (`Identity.UserDelegation.edge`): `Err` refuses it.
pub fn edge(
    provider: DocumentAcp,
    caller: crate::config_client::user_write::Author,
    presented: Presented,
) -> Result<()> {
    use crate::config_client::user_write::Author;
    match (provider, caller, presented) {
        (DocumentAcp::Vera, Author::User, Presented::IdentityToken) => {
            Err(DocumentAcpError::DelegationRequired)
        }
        _ => Ok(()),
    }
}

pub(crate) fn refused(error: &defra_node::Error) -> Option<DocumentAcpError> {
    #[cfg(feature = "vera")]
    {
        embedded_vera::deployment_differs(error)
            .map(|what| DocumentAcpError::DeploymentDiffers { what })
    }
    #[cfg(not(feature = "vera"))]
    {
        let _ = error;
        None
    }
}

pub(crate) fn vera_dir(data: &Path) -> PathBuf {
    data.join(VERA_DIR_NAME)
}

fn node_dir(data: &Path) -> PathBuf {
    vera_dir(data).join(NODE_DIR_NAME)
}

fn recorded_deployment(data: &Path) -> Result<Option<Deployment>> {
    let path = data.join(DEPLOYMENT_RECORD);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(DocumentAcpError::Inspect { path, source }),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| DocumentAcpError::Record { path, source })
}

/// The provider the store at `data` was created with.
pub(crate) fn recorded(data: &Path) -> Result<DocumentAcp> {
    Ok(match recorded_deployment(data)? {
        Some(_) => DocumentAcp::Vera,
        None => DocumentAcp::Local,
    })
}

pub(crate) fn created(
    node: &defra_node::Node,
    requested: DocumentAcp,
) -> Result<Option<Deployment>> {
    match requested {
        DocumentAcp::Local => Ok(None),
        DocumentAcp::Vera => started(node).map(Some).ok_or(DocumentAcpError::Unavailable),
    }
}

/// The caller writes the store's lineage marker only after this returns.
pub(crate) fn record(data: &Path, deployment: &Deployment) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(data.join(DEPLOYMENT_RECORD))?;
    file.write_all(&serde_json::to_vec(deployment)?)?;
    file.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(data)?.sync_all()?;
    Ok(())
}

fn started(node: &defra_node::Node) -> Option<Deployment> {
    #[cfg(feature = "vera")]
    {
        embedded_vera::consensus_key(node).map(|consensus_key| Deployment { consensus_key })
    }
    #[cfg(not(feature = "vera"))]
    {
        let _ = node;
        None
    }
}

#[cfg(feature = "vera")]
fn ensure_present(data: &Path) -> Result<()> {
    let path = node_dir(data);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(DocumentAcpError::DeploymentMissing { path }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(DocumentAcpError::DeploymentMissing { path })
        }
        Err(source) => Err(DocumentAcpError::Inspect { path, source }),
    }
}

fn ensure_recorded(data: &Path, started: Option<&Deployment>) -> Result<()> {
    if recorded_deployment(data)?.as_ref() == started {
        return Ok(());
    }
    Err(DocumentAcpError::DeploymentReplaced {
        path: node_dir(data),
    })
}

/// The caller shuts a refused node down.
pub(crate) fn check_started(
    node: &defra_node::Node,
    data: &Path,
    requested: DocumentAcp,
) -> Result<()> {
    match requested {
        DocumentAcp::Local => Ok(()),
        DocumentAcp::Vera => ensure_recorded(data, started(node).as_ref()),
    }
}

fn signs_for_vera(node: &dyn NodeIdentity) -> bool {
    matches!(
        crypto::parse_did_key(node.did()),
        Ok((crypto::KeyType::Ed25519, _))
    )
}

fn decide(
    recorded: Option<DocumentAcp>,
    requested: DocumentAcp,
    available: bool,
    node_key_signs: bool,
    node: impl FnOnce() -> String,
) -> Result<DocumentAcp> {
    if let Some(recorded) = recorded.filter(|recorded| *recorded != requested) {
        return Err(DocumentAcpError::Switch {
            recorded,
            requested,
        });
    }
    match requested {
        DocumentAcp::Local => Ok(DocumentAcp::Local),
        DocumentAcp::Vera if !available => Err(DocumentAcpError::Unavailable),
        DocumentAcp::Vera if !node_key_signs => {
            Err(DocumentAcpError::NodeKeyCannotSign { node: node() })
        }
        DocumentAcp::Vera => Ok(DocumentAcp::Vera),
    }
}

pub(crate) fn admit(
    data: &Path,
    fresh: bool,
    requested: DocumentAcp,
    node: &dyn NodeIdentity,
    available: bool,
) -> Result<DocumentAcp> {
    let recorded = if fresh { None } else { Some(recorded(data)?) };
    decide(recorded, requested, available, signs_for_vera(node), || {
        format!("node identity {}", node.did())
    })
}

/// [`admit`] for a home's choice as its init config records it, before
/// anything is written or a node is built.
fn enter(
    recorded: Option<DocumentAcp>,
    requested: DocumentAcp,
    available: bool,
    node_key_signs: bool,
) -> Result<DocumentAcp> {
    decide(recorded, requested, available, node_key_signs, || {
        "a Secure Enclave node identity".to_owned()
    })
}

pub fn ensure_admissible(
    recorded: Option<DocumentAcp>,
    requested: DocumentAcp,
    secure_enclave_node_key: bool,
) -> Result<()> {
    enter(
        recorded,
        requested,
        cfg!(feature = "vera"),
        !secure_enclave_node_key,
    )
    .map(|_| ())
}

pub fn ensure_home_opens<ToolPackage, ToolCeiling>(
    config: &StoredInitConfig<ToolPackage, ToolCeiling>,
) -> Result<()> {
    ensure_admissible(
        Some(config.document_acp),
        config.document_acp,
        config.identity_backend.as_deref().map(str::trim) == Some(SECURE_ENCLAVE_IDENTITY_BACKEND),
    )
}

pub fn configure(
    builder: defra_node::NodeBuilder,
    data: &Path,
    key: &StoreKey,
    requested: DocumentAcp,
    node: &Arc<dyn NodeIdentity>,
) -> Result<defra_node::NodeBuilder> {
    apply(builder, data, key, false, requested, node)
}

pub(crate) fn configure_fresh(
    builder: defra_node::NodeBuilder,
    data: &Path,
    key: &StoreKey,
    requested: DocumentAcp,
    node: &Arc<dyn NodeIdentity>,
) -> Result<defra_node::NodeBuilder> {
    apply(builder, data, key, true, requested, node)
}

fn apply(
    builder: defra_node::NodeBuilder,
    data: &Path,
    key: &StoreKey,
    fresh: bool,
    requested: DocumentAcp,
    node: &Arc<dyn NodeIdentity>,
) -> Result<defra_node::NodeBuilder> {
    let available = cfg!(feature = "vera");
    match admit(data, fresh, requested, node.as_ref(), available)? {
        DocumentAcp::Local => Ok(builder),
        #[cfg(feature = "vera")]
        DocumentAcp::Vera => {
            if !fresh {
                ensure_present(data)?;
            }
            Ok(embedded_vera::configure(
                builder,
                &node_dir(data),
                &vera_dir(data),
                key,
                node,
            ))
        }
        #[cfg(not(feature = "vera"))]
        DocumentAcp::Vera => {
            let _ = key;
            Err(DocumentAcpError::Unavailable)
        }
    }
}
