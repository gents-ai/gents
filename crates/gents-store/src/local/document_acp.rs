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
#[cfg(test)]
mod tests;

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
            Self::Local => "the node's own store",
            Self::Vera => "Vera",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentAcpError {
    #[error(
        "this home decides document access through {recorded} and cannot be switched to \
         {requested}; initialize a new home to choose differently"
    )]
    Switch {
        recorded: DocumentAcp,
        requested: DocumentAcp,
    },
    #[error(
        "this home decides document access through an embedded Vera node, which this build does \
         not include; use a build with the `vera` feature"
    )]
    Unavailable,
    #[error(
        "{node} cannot sign for an embedded Vera node, which needs an Ed25519 node key; \
         initialize the home with a file or macos-keychain identity"
    )]
    NodeKeyCannotSign { node: String },
    #[error(
        "a home that decides document access through an embedded Vera node cannot be created or \
         opened yet, because client-signed writes and staged access-control writes are not \
         supported on it; use a home initialized without `--document-acp vera`"
    )]
    NotYetUsable,
    #[error(
        "this home decides document access through Vera, where the runtime never signs with a \
         User key; wire the plugin from the client that holds the key"
    )]
    HeldUserKey,
    #[error(
        "this home's embedded Vera deployment was created under a different {what} than this \
         Gents build and node identity give it; open the home with the Gents version that \
         created it"
    )]
    DeploymentDiffers { what: &'static str },
    #[error(
        "this home decides document access through an embedded Vera node, whose directory {} is \
         missing. It held the access records of the home's documents, so nothing was created in \
         its place; restore it from a backup of this home",
        path.display()
    )]
    DeploymentMissing { path: PathBuf },
    #[error(
        "the embedded Vera deployment in {} is not the one this home was created with, so it \
         holds none of the access records of the home's documents; restore the directory from a \
         backup of this home",
        path.display()
    )]
    DeploymentReplaced { path: PathBuf },
    #[error("reading the embedded Vera deployment record {}", path.display())]
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
    pub(crate) fn refusal(&self) -> Option<IncompatibleStoreKind> {
        match self {
            Self::DeploymentDiffers { .. } => Some(IncompatibleStoreKind::OtherVersionSchema),
            Self::DeploymentMissing { .. }
            | Self::DeploymentReplaced { .. }
            | Self::Record { .. } => Some(IncompatibleStoreKind::UnsupportedOrCorrupt),
            Self::Switch { .. }
            | Self::Unavailable
            | Self::NodeKeyCannotSign { .. }
            | Self::NotYetUsable
            | Self::HeldUserKey
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

fn recorded(data: &Path) -> Result<DocumentAcp> {
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

fn usable(provider: DocumentAcp) -> Result<DocumentAcp> {
    match provider {
        DocumentAcp::Local => Ok(provider),
        DocumentAcp::Vera => Err(DocumentAcpError::NotYetUsable),
    }
}

fn enter(
    recorded: Option<DocumentAcp>,
    requested: DocumentAcp,
    available: bool,
    node_key_signs: bool,
) -> Result<DocumentAcp> {
    decide(recorded, requested, available, node_key_signs, || {
        "a Secure Enclave node identity".to_owned()
    })
    .and_then(usable)
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
