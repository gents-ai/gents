use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;

use super::{home::StoreLock, store_key::StoreKey};

/// The store's lineage marker. Its version names the node grants a store was
/// created under as well as its format: v2 began when the Agent baseline
/// gained `get-collection` (gents#3747, Lean
/// `Identity.GrantSets.widened_baseline_blocks_actorless_reconcile`). A store
/// without this version's marker is reported as made by an older version and
/// is never opened, so nothing in it changes; there is no conversion.
const MARKER: &str = ".gents-private-backend-v2";
pub(crate) const PROVENANCE_MARKER: &str = MARKER;
const DOMAIN: &[u8] = b"gents private backend fresh store v2\0";
type MarkerMac = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum BackendProtectionError {
    #[error("backend protection requires the matching held store lock")]
    LockMismatch,
    #[error("backend protection requires an empty fresh store")]
    NotFresh,
    #[error("fresh backend protection provenance is absent or invalid")]
    InvalidProvenance,
    /// An existing store carries no marker of this version's lineage: it was
    /// created by a release from before backend protection or under an
    /// earlier lineage version, which this release does not convert.
    #[error(
        "this home was created by an older Gents version, which this \
         release cannot open or convert. Back the home up or delete it, then run `gents` to set \
         up a fresh one. To keep anything, export it first with the release that created it."
    )]
    MissingProvenance,
    #[error("legacy backend policy binding is missing or incompatible")]
    InvalidBinding,
    /// Decided on a copy before the store itself was opened, so nothing in
    /// the home was written.
    #[error("the store was checked on a copy and not opened")]
    Incompatible(#[source] gents_protocol::operations::install_repair::IncompatibleStore),
    #[error("backend protection refers to a different native node")]
    WrongNode,
    #[error(
        "node access control is {status}; the runtime operates only with NAC enabled, so \
         re-enable NAC as the node owner or an admin before starting"
    )]
    NodeAcpNotEnabled { status: String },
    #[error("backend protection storage operation failed")]
    Io(#[from] std::io::Error),
    #[error("backend protection native operation failed")]
    Node(#[from] defra_node::Error),
    #[error("backend protection schema initialization failed")]
    Schema(#[source] anyhow::Error),
    #[error("backend protection durable sync failed")]
    Join(#[from] tokio::task::JoinError),
    #[error("the started node could not take its composition root's attachments")]
    Composition(#[from] crate::node_extension::ExtensionError),
}

impl BackendProtectionError {
    pub fn incompatible(
        &self,
    ) -> Option<&gents_protocol::operations::install_repair::IncompatibleStore> {
        match self {
            Self::Incompatible(store) => Some(store),
            _ => None,
        }
    }
}

type Result<T> = std::result::Result<T, BackendProtectionError>;

/// Refuses a store that carries an earlier lineage's marker and not this
/// version's, by reading the directory's names only. It runs before the
/// store lock is taken, because taking the lock rewrites the lock file and a
/// refused older home is left byte-identical. Only a positively earlier
/// marker is refused here: a store with no marker at all may be one another
/// process is still initializing, which the lock decides.
pub fn refuse_earlier_lineage(data: &Path) -> Result<()> {
    let prefix = MARKER.rsplit_once('v').map_or(MARKER, |(prefix, _)| prefix);
    let mut earlier = false;
    for entry in std::fs::read_dir(data)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name == MARKER {
            return Ok(());
        }
        earlier |= name.starts_with(prefix);
    }
    if earlier {
        return Err(BackendProtectionError::MissingProvenance);
    }
    Ok(())
}

async fn check_lock(data: &Path, lock: &StoreLock) -> Result<PathBuf> {
    let path = tokio::fs::canonicalize(data).await?;
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(BackendProtectionError::LockMismatch);
    };
    let expected = parent.join(format!("{}.lock", name.to_string_lossy()));
    if lock.path() != expected {
        return Err(BackendProtectionError::LockMismatch);
    }
    Ok(path)
}

/// A fresh origin is observed while the caller holds the store's actual lock.
/// The caller retains that lock through initialization and subsequent use.
pub struct FreshBackendStore<'a> {
    data: PathBuf,
    _lock: &'a StoreLock,
}

impl<'a> FreshBackendStore<'a> {
    pub async fn observe(data: &Path, lock: &'a StoreLock) -> Result<Self> {
        let data = check_lock(data, lock).await?;
        if tokio::fs::read_dir(&data)
            .await?
            .next_entry()
            .await?
            .is_some()
        {
            return Err(BackendProtectionError::NotFresh);
        }
        Ok(Self { data, _lock: lock })
    }

    /// The temporary native Node has no HTTP or P2P transport. An interrupted
    /// initialization or partial marker is never promoted on a later open.
    pub async fn initialize(self, key: &StoreKey, node_did: Option<&str>) -> Result<()> {
        let did = node_did
            .filter(|did| !did.is_empty())
            .ok_or(BackendProtectionError::InvalidProvenance)?;
        let mut builder = super::store_key::persistent_builder(&self.data, key)
            .map_err(|error| BackendProtectionError::Schema(error.into()))?;
        builder = builder
            .with_node_identity_did(did.to_owned())
            .with_node_acp_enabled();
        let node = builder.build().await?;
        let result = async {
            crate::schema::ensure_runtime_schemas(&node)
                .await
                .map_err(BackendProtectionError::Schema)?;
            validate_binding(&node).await
        }
        .await;
        let shutdown = node.shutdown().await;
        result?;
        shutdown?;
        drop(node);
        let nonce = *uuid::Uuid::new_v4().as_bytes();
        let mut mac = key
            .with_secret(|secret| MarkerMac::new_from_slice(secret).expect("HMAC accepts any key"));
        mac.update(DOMAIN);
        mac.update(&nonce);
        let mut bytes = Vec::from(DOMAIN);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.data.join(MARKER))
            .await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        // A directory handle is unix-only: Windows refuses `File::open` on a
        // directory (os error 5), and NTFS journals the new entry itself.
        #[cfg(unix)]
        {
            let data = self.data.clone();
            tokio::task::spawn_blocking(move || std::fs::File::open(data)?.sync_all()).await??;
        }
        Ok(())
    }
}

/// Trusted local storage must retain this marker with the same encrypted data
/// directory. Replacing individual store files is outside that premise.
pub struct ProtectedBackendStore<'a> {
    data: PathBuf,
    _lock: &'a StoreLock,
}

impl<'a> ProtectedBackendStore<'a> {
    pub async fn open(data: &Path, key: &StoreKey, lock: &'a StoreLock) -> Result<Self> {
        let data = check_lock(data, lock).await?;
        let bytes =
            tokio::fs::read(data.join(MARKER))
                .await
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => BackendProtectionError::MissingProvenance,
                    _ => BackendProtectionError::InvalidProvenance,
                })?;
        if bytes.len() != DOMAIN.len() + 16 + 32 || !bytes.starts_with(DOMAIN) {
            return Err(BackendProtectionError::InvalidProvenance);
        }
        let mut mac = key
            .with_secret(|secret| MarkerMac::new_from_slice(secret).expect("HMAC accepts any key"));
        mac.update(&bytes[..DOMAIN.len() + 16]);
        mac.verify_slice(&bytes[DOMAIN.len() + 16..])
            .map_err(|_| BackendProtectionError::InvalidProvenance)?;
        Ok(Self { data, _lock: lock })
    }

    /// Check the current binding before opening any network listener.
    pub async fn preflight(
        self,
        key: &StoreKey,
        node_did: Option<&str>,
    ) -> Result<PreflightProtectedBackendStore<'a>> {
        let did = node_did
            .filter(|did| !did.is_empty())
            .ok_or(BackendProtectionError::InvalidProvenance)?;
        let stamped = super::store_compat::stamped(&self.data);
        if !stamped {
            match super::store_compat::check_copy(&self.data, key).await {
                Ok(None) => {}
                Ok(Some(kind)) => {
                    return Err(BackendProtectionError::Incompatible(
                        gents_protocol::operations::install_repair::IncompatibleStore {
                            kind,
                            data_path: self.data.clone(),
                        },
                    ))
                }
                Err(error) => tracing::warn!(
                    error = %gents_protocol::error_chain::ErrorChain(&error),
                    "could not check a copy of the store first; opening it directly"
                ),
            }
        }
        let mut builder = super::store_key::persistent_builder(&self.data, key)
            .map_err(|error| BackendProtectionError::Schema(error.into()))?;
        builder = builder
            .with_node_identity_did(did.to_owned())
            .with_node_acp_enabled();
        let node = builder.build().await?;
        let checked = validate_binding(&node).await;
        let shutdown = node.shutdown().await;
        checked.map_err(|error| match error {
            BackendProtectionError::InvalidBinding => BackendProtectionError::Incompatible(
                gents_protocol::operations::install_repair::IncompatibleStore {
                    kind: gents_protocol::operations::install_repair::IncompatibleStoreKind::OtherVersionSchema,
                    data_path: self.data.clone(),
                },
            ),
            error => error,
        })?;
        shutdown?;
        if !stamped {
            super::store_compat::record_compatible(&self.data)?;
        }
        Ok(PreflightProtectedBackendStore {
            store: self,
            node_did: did.to_owned(),
        })
    }
}

pub struct PreflightProtectedBackendStore<'a> {
    store: ProtectedBackendStore<'a>,
    node_did: String,
}

impl PreflightProtectedBackendStore<'_> {
    /// Construct the served node from the same locked data directory whose
    /// provenance was checked. Callers cannot substitute another native node.
    /// The caller reads `plugins` from its plugin store while it holds that
    /// store's lease, and keeps the lease until this returns. The node is
    /// returned already carrying `composition`, so no caller can serve
    /// requests from a node its lifecycle points cannot find.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_node_with_projection(
        self,
        key: &StoreKey,
        runtime: Arc<dyn crate::identity::NodeIdentity>,
        http: Option<defra_node::HttpConfig>,
        p2p: Option<defra_node::P2PConfig>,
        policy: Arc<dyn super::ToolSourcePolicy>,
        plugins: Vec<super::ProjectedPlugin>,
        composition: &crate::node_extension::Extensions,
    ) -> Result<(Arc<defra_node::Node>, BackendAdmission)> {
        if runtime.did() != self.node_did {
            return Err(BackendProtectionError::WrongNode);
        }
        let started = self
            .start_node_with_plugins(key, runtime, http, p2p, policy, plugins)
            .await?;
        if let Err(error) = crate::node_extension::attach_all(&started.0, composition) {
            let _ = started.0.shutdown().await;
            return Err(error.into());
        }
        Ok(started)
    }

    async fn start_node_with_plugins(
        self,
        key: &StoreKey,
        runtime: Arc<dyn crate::identity::NodeIdentity>,
        http: Option<defra_node::HttpConfig>,
        p2p: Option<defra_node::P2PConfig>,
        policy: Arc<dyn super::ToolSourcePolicy>,
        plugins: Vec<super::ProjectedPlugin>,
    ) -> Result<(Arc<defra_node::Node>, BackendAdmission)> {
        let projection = super::document_projector_with_installed_plugins(runtime, policy, plugins)
            .map_err(|error| BackendProtectionError::Schema(error.into()))?;
        let mut builder = super::store_key::persistent_builder(&self.store.data, key)
            .map_err(|error| BackendProtectionError::Schema(error.into()))?
            .with_document_projector(projection)
            .with_node_identity_did(self.node_did)
            .with_node_acp_enabled();
        if let Some(http) = http {
            builder = builder.with_http(http);
        }
        if let Some(p2p) = p2p {
            builder = builder.with_p2p(p2p);
        }
        let node = Arc::new(builder.build().await?);
        if let Err(error) = validate_binding(&node).await {
            let _ = node.shutdown().await;
            return Err(error);
        }
        let admission = BackendAdmission {
            node: Arc::downgrade(&node),
        };
        Ok((node, admission))
    }
}

#[derive(Clone)]
pub struct BackendAdmission {
    node: std::sync::Weak<defra_node::Node>,
}

impl BackendAdmission {
    pub async fn validate(&self, node: &Arc<defra_node::Node>) -> Result<()> {
        if !self
            .node
            .upgrade()
            .is_some_and(|bound| Arc::ptr_eq(&bound, node))
        {
            return Err(BackendProtectionError::WrongNode);
        }
        validate_binding(node).await
    }
}

/// `Identity.NodeProvisioning.runtimeMayStart`: every authorization decision
/// assumes NAC is enabled, and native treats every actor as admin while it is
/// not, so a node observed without it is refused before it serves or admits
/// work. This is the one runtime check of that invariant.
async fn require_node_acp_enabled(node: &defra_node::Node) -> Result<()> {
    let nac = node.nac_manager();
    if nac.is_enabled().await {
        return Ok(());
    }
    Err(BackendProtectionError::NodeAcpNotEnabled {
        status: nac.info().await.status,
    })
}

async fn validate_binding(node: &defra_node::Node) -> Result<()> {
    require_node_acp_enabled(node).await?;
    validate_policy_binding(node).await
}

/// The collection binding alone, for a copy of a store opened without its
/// node's identity, which [`require_node_acp_enabled`] needs.
pub(crate) async fn validate_policy_binding(node: &defra_node::Node) -> Result<()> {
    use gents_protocol::schemas::{AGENT_OWNERSHIP_POLICY_ID, INFERENCE_BACKEND_NAME};
    let current = node
        .get_collection(INFERENCE_BACKEND_NAME)?
        .ok_or(BackendProtectionError::InvalidBinding)?;
    let versions = node.get_all_collection_versions().await?;
    let mut matched = false;
    for version in versions.iter().filter(|version| {
        version.name == INFERENCE_BACKEND_NAME || version.collection_id == current.collection_id
    }) {
        matched = true;
        if version.is_branchable
            || !version.policy.as_ref().is_some_and(|policy| {
                policy.id == AGENT_OWNERSHIP_POLICY_ID
                    && policy.resource_name == "secret_configuration"
            })
        {
            return Err(BackendProtectionError::InvalidBinding);
        }
    }
    if !matched {
        return Err(BackendProtectionError::InvalidBinding);
    }
    Ok(())
}
