use std::path::{Path, PathBuf};
use std::sync::Arc;

use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use serde::{Deserialize, Serialize};

use crate::identity::NodeIdentity;
use crate::local::store_key::StoreKey;

#[cfg(feature = "embedded-vera")]
mod embedded_vera;
#[cfg(test)]
mod tests;

/// Holds everything an embedded Vera node keeps, inside the store's data
/// directory: the store's lock, lineage marker, retirement and backups cover
/// it with the store.
const VERA_DIR_NAME: &str = "vera";
const NODE_DIR_NAME: &str = "node";
/// Written once, before the store's lineage marker, so every store that
/// carries the marker and was created on an embedded Vera node has it.
const DEPLOYMENT_RECORD: &str = ".gents-embedded-vera-deployment";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Deployment {
    consensus_key: String,
}

/// `DocumentAcpChoice.Provider`: chosen when a home is initialized, recorded
/// in its init config and never changed afterwards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentAcp {
    #[default]
    Local,
    EmbeddedVera,
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
            Self::EmbeddedVera => "an embedded Vera node",
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
         not include; use a build with the `embedded-vera` feature"
    )]
    Unavailable,
    #[error(
        "node identity {did} cannot sign for an embedded Vera node, which needs an Ed25519 node \
         key; initialize the home with a file or macos-keychain identity"
    )]
    NodeKeyCannotSign { did: String },
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
    /// `DocumentAcpChoice.Reopening.refusal`.
    pub(crate) fn refusal(&self) -> Option<IncompatibleStoreKind> {
        match self {
            Self::DeploymentDiffers { .. } => Some(IncompatibleStoreKind::OtherVersionSchema),
            Self::DeploymentMissing { .. }
            | Self::DeploymentReplaced { .. }
            | Self::Record { .. } => Some(IncompatibleStoreKind::UnsupportedOrCorrupt),
            Self::Switch { .. }
            | Self::Unavailable
            | Self::NodeKeyCannotSign { .. }
            | Self::Inspect { .. } => None,
        }
    }
}

type Result<T> = std::result::Result<T, DocumentAcpError>;

/// `DocumentAcpChoice.reopen`, read from what the node's start returned.
pub(crate) fn refused(error: &defra_node::Error) -> Option<DocumentAcpError> {
    #[cfg(feature = "embedded-vera")]
    {
        embedded_vera::deployment_differs(error)
            .map(|what| DocumentAcpError::DeploymentDiffers { what })
    }
    #[cfg(not(feature = "embedded-vera"))]
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
        Some(_) => DocumentAcp::EmbeddedVera,
        None => DocumentAcp::Local,
    })
}

/// The deployment a fresh store's node created, for [`record`]. `None` for a
/// store that decides document access itself.
pub(crate) fn created(
    node: &defra_node::Node,
    requested: DocumentAcp,
) -> Result<Option<Deployment>> {
    match requested {
        DocumentAcp::Local => Ok(None),
        DocumentAcp::EmbeddedVera => started(node).map(Some).ok_or(DocumentAcpError::Unavailable),
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
    #[cfg(feature = "embedded-vera")]
    {
        embedded_vera::consensus_key(node).map(|consensus_key| Deployment { consensus_key })
    }
    #[cfg(not(feature = "embedded-vera"))]
    {
        let _ = node;
        None
    }
}

#[cfg(feature = "embedded-vera")]
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

/// `DocumentAcpChoice.reopen`, once a node [`configure`] prepared has started.
/// The caller shuts a refused node down.
pub(crate) fn check_started(
    node: &defra_node::Node,
    data: &Path,
    requested: DocumentAcp,
) -> Result<()> {
    match requested {
        DocumentAcp::Local => Ok(()),
        DocumentAcp::EmbeddedVera => ensure_recorded(data, started(node).as_ref()),
    }
}

fn signs_for_vera(node: &dyn NodeIdentity) -> bool {
    matches!(
        crypto::parse_did_key(node.did()),
        Ok((crypto::KeyType::Ed25519, _))
    )
}

/// `DocumentAcpChoice.admit`. A fresh store has no record and takes the
/// request; `available` is whether this build carries the Vera node.
pub(crate) fn admit(
    data: &Path,
    fresh: bool,
    requested: DocumentAcp,
    node: &dyn NodeIdentity,
    available: bool,
) -> Result<DocumentAcp> {
    if !fresh {
        let recorded = recorded(data)?;
        if recorded != requested {
            return Err(DocumentAcpError::Switch {
                recorded,
                requested,
            });
        }
    }
    match requested {
        DocumentAcp::Local => Ok(DocumentAcp::Local),
        DocumentAcp::EmbeddedVera if !available => Err(DocumentAcpError::Unavailable),
        DocumentAcp::EmbeddedVera if !signs_for_vera(node) => {
            Err(DocumentAcpError::NodeKeyCannotSign {
                did: node.did().to_owned(),
            })
        }
        DocumentAcp::EmbeddedVera => Ok(DocumentAcp::EmbeddedVera),
    }
}

/// Refuses a choice this build or this node's key cannot serve, before a
/// caller records it.
pub fn ensure_admitted(
    data: &Path,
    fresh: bool,
    requested: DocumentAcp,
    node: &dyn NodeIdentity,
) -> Result<()> {
    admit(
        data,
        fresh,
        requested,
        node,
        cfg!(feature = "embedded-vera"),
    )
    .map(|_| ())
}

/// Gives `builder` the document access control an existing store was created
/// with. Every node opened on a home's store as its Node goes through here or
/// through the backend protection owners, which call it.
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
    let available = cfg!(feature = "embedded-vera");
    match admit(data, fresh, requested, node.as_ref(), available)? {
        DocumentAcp::Local => Ok(builder),
        #[cfg(feature = "embedded-vera")]
        DocumentAcp::EmbeddedVera => {
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
        #[cfg(not(feature = "embedded-vera"))]
        DocumentAcp::EmbeddedVera => {
            let _ = key;
            Err(DocumentAcpError::Unavailable)
        }
    }
}
