use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::identity::NodeIdentity;
use crate::local::store_key::StoreKey;

#[cfg(feature = "embedded-vera")]
mod embedded_vera;
#[cfg(test)]
mod tests;

/// Holds everything an embedded Vera node keeps, inside the store's data
/// directory: the store's lock, lineage marker, retirement and backups cover
/// it with the store. Its presence is the store's record of the choice.
const VERA_DIR_NAME: &str = "vera";

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
    #[error("inspecting {}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, DocumentAcpError>;

pub(crate) fn vera_dir(data: &Path) -> PathBuf {
    data.join(VERA_DIR_NAME)
}

fn recorded(data: &Path) -> Result<DocumentAcp> {
    let path = vera_dir(data);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(DocumentAcp::EmbeddedVera),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentAcp::Local),
        Err(source) => Err(DocumentAcpError::Inspect { path, source }),
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
        DocumentAcp::EmbeddedVera => Ok(embedded_vera::configure(builder, data, key, node)),
        #[cfg(not(feature = "embedded-vera"))]
        DocumentAcp::EmbeddedVera => {
            let _ = key;
            Err(DocumentAcpError::Unavailable)
        }
    }
}
