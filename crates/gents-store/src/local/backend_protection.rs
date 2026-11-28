use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;

use super::{
    document_acp::{self, DocumentAcp, DocumentAcpError},
    home::StoreLock,
    store_key::StoreKey,
};
use crate::identity::NodeIdentity;

/// This build's lineage marker (Lean `BackendPreflight.markerPrefix`):
/// [`MARKER_PREFIX`] followed by [`VERSION`]. A new lineage bumps this name,
/// [`VERSION`] and [`DOMAIN`] together.
const MARKER: &str = ".gents-private-backend-v3";
const MARKER_PREFIX: &str = ".gents-private-backend-v";
const VERSION: u64 = 3;
pub const PROVENANCE_MARKER: &str = MARKER;
const DOMAIN: &[u8] = b"gents private backend fresh store v3\0";
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
    /// An existing store carries a later lineage's marker, or one whose
    /// version this release cannot read, and not this version's: a newer
    /// release made it and may still want it.
    #[error(
        "this home was created by a newer Gents version, which this release cannot open. \
         Nothing in it was changed. Open it with that newer version of Gents."
    )]
    NewerLineage,
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
    #[error(
        "the runtime's Node identity reads past document access control: it owns node access \
         control, is a node admin or holds bypass-dac, so it would read every actor's documents. \
         A home set up before its User owned node access control cannot be converted: back it up \
         or delete it, then run `gents` to set up a fresh one. Otherwise revoke the Node's admin \
         or bypass-dac as the home's User."
    )]
    NodeReadsPastDocumentAcp,
    #[error("node access control could not be read")]
    NodeAcpRead(#[source] db::nac::Error),
    #[error("node access control must be owned by the home's User, not its Node")]
    NodeOwnedNodeAcp,
    #[error("the requested transport account differs from the shared home’s account")]
    WrongTransportAccount,
    #[error("the shared Vera operator must be the home’s account")]
    WrongVeraAccount,
    #[error("admitted device requires the signed registration returned by admit-device")]
    MissingPublisherNode,
    #[error("the store could not be checked on a copy; the original was not opened")]
    CopyCheck(#[source] Box<super::store_compat::StoreCompatibilityError>),
    #[error("backend protection storage operation failed")]
    Io(#[from] std::io::Error),
    #[error("backend protection native operation failed")]
    Node(#[from] defra_node::Error),
    #[error(transparent)]
    Home(#[from] super::home::HomeError),
    #[error("backend protection schema initialization failed")]
    Schema(#[source] anyhow::Error),
    #[error("backend protection durable sync failed")]
    Join(#[from] tokio::task::JoinError),
    #[error("the started node could not take its composition root's attachments")]
    Composition(#[from] crate::node_extension::ExtensionError),
    #[error("this device has no certified group assignment from transport account {0}")]
    DeviceNotAdmitted(identity::Did),
    #[error("reading the account device group during preflight")]
    DeviceGroup(#[source] Box<crate::config_client::DeviceGroupReadError>),
    #[error(transparent)]
    DocumentAcp(#[from] DocumentAcpError),
}

impl From<crate::config_client::DeviceGroupReadError> for BackendProtectionError {
    fn from(error: crate::config_client::DeviceGroupReadError) -> Self {
        Self::DeviceGroup(Box::new(error))
    }
}

impl BackendProtectionError {
    pub(crate) fn in_chain<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        gents_protocol::error_chain::find_source::<Self>(error)
    }

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

/// Lean `BackendPreflight.refusal` of `BackendPreflight.lineage`: what the
/// lineage markers among a store directory's `names` refuse it as, for a build
/// of lineage `current`, read before the store lock (`before_lock`) or not.
pub(crate) fn lineage_refusal(
    current: u64,
    names: &[String],
    before_lock: bool,
) -> Option<BackendProtectionError> {
    let (mut marked, mut newer) = (false, false);
    for suffix in names
        .iter()
        .filter_map(|name| name.strip_prefix(MARKER_PREFIX))
    {
        match suffix
            .parse::<u64>()
            .ok()
            .filter(|version| version.to_string() == suffix)
        {
            Some(version) if version == current => return None,
            Some(version) => newer |= version > current,
            None => newer = true,
        }
        marked = true;
    }
    if newer {
        Some(BackendProtectionError::NewerLineage)
    } else if marked || !before_lock {
        Some(BackendProtectionError::MissingProvenance)
    } else {
        None
    }
}

/// Refuses the store at `data` by its lineage markers, reading the
/// directory's names only.
pub(crate) fn refuse_lineage(data: &Path, before_lock: bool) -> Result<()> {
    let names = std::fs::read_dir(data)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    lineage_refusal(VERSION, &names, before_lock).map_or(Ok(()), Err)
}

/// Refuses a store that carries another lineage's marker and not this
/// version's. It runs before the store lock is taken, because taking the lock
/// rewrites the lock file and a refused home is left byte-identical.
pub fn refuse_other_lineage(data: &Path) -> Result<()> {
    refuse_lineage(data, true)
}

fn node_did(node: &Arc<dyn NodeIdentity>) -> Result<String> {
    let did = node.did();
    if did.is_empty() {
        return Err(BackendProtectionError::InvalidProvenance);
    }
    Ok(did.to_owned())
}

/// Builds the node over the store at `data`, whose lock the caller holds,
/// once the engine of any node it built there before has released the
/// directory (`home::wait_for_engine_release`).
pub async fn build_configured(
    builder: defra_node::NodeBuilder,
    data: &Path,
    document_acp: DocumentAcp,
) -> Result<Arc<defra_node::Node>> {
    let record = document_acp::recorded_deployment(data)?;
    let ids =
        document_acp::policy_ids_for_connection(record.as_ref().map(|record| &record.connection));
    super::home::wait_for_engine_release(data).await?;
    let node = Arc::new(start(builder).await?);
    let configure = || -> Result<()> {
        document_acp::check_started(&node, data, document_acp)?;
        let mut extensions =
            crate::node_extension::Extensions::of(Arc::new(document_acp::BoundPolicyIds(ids)));
        if let Some(record) = record {
            use document_acp::VeraConnection;
            match (record.connection, record.transport) {
                (VeraConnection::Solo, None) => {}
                (VeraConnection::Host { account, .. }, Some(scope))
                    if scope.account == account && scope.host_node == scope.device_node =>
                {
                    extensions.attach(Arc::new(scope))?;
                }
                (
                    VeraConnection::Follower {
                        anchor,
                        device_registration: Some(evidence),
                        ..
                    },
                    Some(scope),
                ) if scope.account == anchor.account
                    && scope.host_node == anchor.host_node
                    && scope.group == anchor.group =>
                {
                    let device = identity::Did::new(
                        node.node_identity_did()
                            .ok_or(BackendProtectionError::WrongNode)?,
                    )
                    .map_err(|_| BackendProtectionError::WrongNode)?;
                    let registered = crate::config_client::verified_publisher_node(
                        evidence,
                        &device,
                        &anchor.account,
                    )
                    .map_err(|error| BackendProtectionError::Schema(error.into()))?;
                    if registered != scope.device_node {
                        return Err(BackendProtectionError::InvalidProvenance);
                    }
                    extensions.attach(Arc::new(scope))?;
                }
                _ => return Err(BackendProtectionError::InvalidProvenance),
            }
        }
        crate::node_extension::attach_all(&node, &extensions)?;
        Ok(())
    };
    if let Err(error) = configure() {
        let _ = node.shutdown().await;
        return Err(error);
    }
    Ok(node)
}

async fn start(builder: defra_node::NodeBuilder) -> Result<defra_node::Node> {
    builder
        .build()
        .await
        .map_err(|error| match document_acp::refused(&error) {
            Some(refused) => refused.into(),
            None => error.into(),
        })
}

pub(crate) fn checked_copy(
    data: &Path,
    verdict: std::result::Result<
        Option<gents_protocol::operations::install_repair::IncompatibleStoreKind>,
        super::store_compat::StoreCompatibilityError,
    >,
) -> Result<()> {
    match verdict {
        Ok(None) => Ok(()),
        Ok(Some(kind)) => Err(BackendProtectionError::Incompatible(
            gents_protocol::operations::install_repair::IncompatibleStore {
                kind,
                data_path: data.to_path_buf(),
            },
        )),
        Err(error) => Err(BackendProtectionError::CopyCheck(Box::new(error))),
    }
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
    durability: storage::backends::DurabilityMode,
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
        Ok(Self {
            data,
            _lock: lock,
            durability: storage::backends::DurabilityMode::Immediate,
        })
    }

    #[must_use]
    pub fn with_durability(mut self, durability: storage::backends::DurabilityMode) -> Self {
        self.durability = durability;
        self
    }

    /// The temporary native Node has no HTTP or P2P transport. An interrupted
    /// initialization or partial marker is never promoted on a later open.
    pub async fn initialize(
        self,
        key: &StoreKey,
        node: &Arc<dyn NodeIdentity>,
        nac_owner: &identity::Did,
        document_acp: DocumentAcp,
    ) -> Result<()> {
        self.initialize_with_connection(
            key,
            node,
            nac_owner,
            document_acp,
            &document_acp::VeraConnection::Solo,
            None,
        )
        .await
    }

    pub async fn initialize_with_connection(
        self,
        key: &StoreKey,
        node: &Arc<dyn NodeIdentity>,
        nac_owner: &identity::Did,
        document_acp: DocumentAcp,
        connection: &document_acp::VeraConnection,
        account_key: Option<crate::config_client::ClientSigningCapability>,
    ) -> Result<()> {
        let did = node_did(node)?;
        if nac_owner.as_str() == did {
            return Err(BackendProtectionError::NodeOwnedNodeAcp);
        }
        if matches!(connection, document_acp::VeraConnection::Host { account, .. } if account != nac_owner)
        {
            return Err(BackendProtectionError::WrongVeraAccount);
        }
        let builder = super::store_key::persistent_builder(
            &self.data,
            key,
            crate::local::storage_backend::DEFAULT_BLOCK_CACHE_BYTES,
        )
        .map_err(|error| BackendProtectionError::Schema(error.into()))?
        .with_storage_durability(self.durability)
        .with_node_identity_did(did.clone())
        .with_node_acp_owner(nac_owner.as_str());
        let builder = document_acp::configure_fresh(
            builder,
            &self.data,
            key,
            document_acp,
            node,
            connection,
        )?;
        let (builder, host_setup) =
            if let document_acp::VeraConnection::Host { account, .. } = connection {
                let signer = account_key
                    .filter(|key| key.did() == account.as_str())
                    .ok_or(BackendProtectionError::WrongVeraAccount)?;
                let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
                let url = format!("http://{}/api/v0/graphql", listener.local_addr()?);
                let route = crate::config_client::OwnerStagedRoute::new();
                let fence_route = route.clone();
                let http = defra_node::HttpConfig::with_listener(listener)?
                    .with_authenticated_routes(route.router())
                    .with_router_transform(Arc::new(move |router| {
                        fence_route.wrap_relationship_fence(router)
                    }));
                (
                    builder.with_http(http),
                    Some((url, signer, route, node.clone())),
                )
            } else {
                (builder, None)
            };
        let node = Arc::new(start(builder).await?);
        let result: Result<_> = async {
            let ids = document_acp::policy_ids_for_connection(
                (document_acp == DocumentAcp::Vera).then_some(connection),
            );
            crate::node_extension::attach_all(
                &node,
                &crate::node_extension::Extensions::of(Arc::new(document_acp::BoundPolicyIds(ids))),
            )?;
            let mut deployment = document_acp::created(&node, document_acp, connection)?;
            grant_node_runtime(&node, nac_owner, &did).await?;
            if let document_acp::VeraConnection::Follower { anchor, .. } = connection {
                require_device_group(&node, &did, &anchor.account).await?;
            }
            if !matches!(connection, document_acp::VeraConnection::Solo) {
                crate::schema::catalog::ensure_admission_schema(&node)
                    .await
                    .map_err(|error| BackendProtectionError::Schema(error.into()))?;
                let scope = match (connection, host_setup) {
                    (
                        document_acp::VeraConnection::Host { account, .. },
                        Some((url, signer, route, runtime)),
                    ) => {
                        route.serve(&node, runtime);
                        Box::pin(crate::schema::catalog::SharedTransportHome::create_host(
                            &node, url, signer, account,
                        ))
                        .await
                        .map_err(|error| BackendProtectionError::Schema(error.into()))?
                    }
                    (
                        document_acp::VeraConnection::Follower {
                            anchor,
                            device_registration,
                            ..
                        },
                        None,
                    ) => crate::schema::catalog::SharedTransportHome {
                        account: anchor.account.clone(),
                        group: anchor.group.clone(),
                        host_node: anchor.host_node.clone(),
                        device_node: crate::config_client::verified_publisher_node(
                            device_registration
                                .clone()
                                .ok_or(BackendProtectionError::MissingPublisherNode)?,
                            &identity::Did::new(&did)
                                .map_err(|_| BackendProtectionError::WrongNode)?,
                            &anchor.account,
                        )
                        .map_err(|error| BackendProtectionError::Schema(error.into()))?,
                    },
                    _ => return Err(BackendProtectionError::InvalidProvenance),
                };
                deployment
                    .as_mut()
                    .ok_or(BackendProtectionError::InvalidProvenance)?
                    .transport = Some(scope.clone());
                crate::node_extension::attach_all(
                    &node,
                    &crate::node_extension::Extensions::of(Arc::new(scope)),
                )?;
            }
            crate::schema::ensure_runtime_schemas(&node)
                .await
                .map_err(BackendProtectionError::Schema)?;
            validate_binding(&node).await?;
            Ok((deployment, ids))
        }
        .await;
        let shutdown = node.shutdown().await;
        let (deployment, ids) = result?;
        shutdown?;
        drop(node);
        if let Some(deployment) = deployment {
            let data = self.data.clone();
            tokio::task::spawn_blocking(move || document_acp::record(&data, &deployment)).await??;
        }
        super::store_compat::record_compatible(&self.data, ids)?;
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
        // tokio::fs::File completes writes on a blocking thread; flush waits
        // for the OS write to finish before drop when sync_all is skipped.
        file.flush().await?;
        if self.durability == storage::backends::DurabilityMode::Immediate {
            file.sync_all().await?;
            // A directory handle is unix-only: Windows refuses `File::open` on a
            // directory (os error 5), and NTFS journals the new entry itself.
            #[cfg(unix)]
            {
                let data = self.data.clone();
                tokio::task::spawn_blocking(move || std::fs::File::open(data)?.sync_all())
                    .await??;
            }
        }
        Ok(())
    }
}

/// Trusted local storage must retain this marker with the same encrypted data
/// directory. Replacing individual store files is outside that premise.
pub struct ProtectedBackendStore<'a> {
    data: PathBuf,
    _lock: &'a StoreLock,
    durability: storage::backends::DurabilityMode,
}

impl<'a> ProtectedBackendStore<'a> {
    pub async fn open(data: &Path, key: &StoreKey, lock: &'a StoreLock) -> Result<Self> {
        let data = check_lock(data, lock).await?;
        let listed = data.clone();
        tokio::task::spawn_blocking(move || refuse_lineage(&listed, false)).await??;
        let bytes = tokio::fs::read(data.join(MARKER))
            .await
            .map_err(|_| BackendProtectionError::InvalidProvenance)?;
        if bytes.len() != DOMAIN.len() + 16 + 32 || !bytes.starts_with(DOMAIN) {
            return Err(BackendProtectionError::InvalidProvenance);
        }
        let mut mac = key
            .with_secret(|secret| MarkerMac::new_from_slice(secret).expect("HMAC accepts any key"));
        mac.update(&bytes[..DOMAIN.len() + 16]);
        mac.verify_slice(&bytes[DOMAIN.len() + 16..])
            .map_err(|_| BackendProtectionError::InvalidProvenance)?;
        Ok(Self {
            data,
            _lock: lock,
            durability: storage::backends::DurabilityMode::Immediate,
        })
    }

    #[must_use]
    pub fn with_durability(mut self, durability: storage::backends::DurabilityMode) -> Self {
        self.durability = durability;
        self
    }

    /// Check the current binding before opening any network listener.
    pub async fn preflight(
        self,
        key: &StoreKey,
        node: &Arc<dyn NodeIdentity>,
        document_acp: DocumentAcp,
    ) -> Result<PreflightProtectedBackendStore<'a>> {
        self.preflight_with_account(key, node, document_acp, None)
            .await
    }

    /// Use the expected account's certified group when configuring a Vera
    /// receiver. Complete the lookup before opening any network listener.
    pub async fn preflight_with_account(
        self,
        key: &StoreKey,
        node: &Arc<dyn NodeIdentity>,
        document_acp: DocumentAcp,
        account: Option<&identity::Did>,
    ) -> Result<PreflightProtectedBackendStore<'a>> {
        let did = node_did(node)?;
        let connection = document_acp::recorded_connection(&self.data)?;
        let shared_account = match &connection {
            Some(document_acp::VeraConnection::Host { account, .. }) => Some(account),
            Some(document_acp::VeraConnection::Follower { anchor, .. }) => Some(&anchor.account),
            _ => None,
        };
        if matches!((account, shared_account), (Some(requested), Some(bound)) if requested != bound)
        {
            return Err(BackendProtectionError::WrongTransportAccount);
        }
        let account = shared_account.or(account);
        let ids = document_acp::policy_ids_for_connection(connection.as_ref());
        let stamped = super::store_compat::stamped(&self.data, ids);
        if !stamped {
            checked_copy(
                &self.data,
                super::store_compat::check_copy(&self.data, key, ids).await,
            )?;
        }
        let builder = super::store_key::persistent_builder(
            &self.data,
            key,
            crate::local::storage_backend::DEFAULT_BLOCK_CACHE_BYTES,
        )
        .map_err(|error| BackendProtectionError::Schema(error.into()))?
        .with_storage_durability(self.durability)
        .with_node_identity_did(did.clone())
        .with_node_acp_enabled();
        let builder =
            document_acp::configure_preflight(builder, &self.data, key, document_acp, node)?;
        let node = build_configured(builder, &self.data, document_acp).await?;
        let checked = async {
            validate_binding(&node).await?;
            match account {
                None => Ok(None),
                Some(account) => {
                    let group = require_device_group(&node, &did, account).await?;
                    if shared_account.is_some() {
                        crate::schema::catalog::verify_installed_transport(&node)
                            .await
                            .map_err(|error| BackendProtectionError::Schema(error.into()))?;
                    }
                    Ok(Some(group))
                }
            }
        }
        .await;
        let shutdown = node.shutdown().await;
        let transport_replica = checked.map_err(|error| match error {
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
            super::store_compat::record_compatible(&self.data, ids)?;
        }
        Ok(PreflightProtectedBackendStore {
            store: self,
            transport_replica,
            node_did: did,
            document_acp,
            block_cache_bytes: crate::local::storage_backend::DEFAULT_BLOCK_CACHE_BYTES,
        })
    }
}

pub struct PreflightProtectedBackendStore<'a> {
    store: ProtectedBackendStore<'a>,
    transport_replica: Option<crate::config_client::transport_replica::TransportReplica>,
    node_did: String,
    document_acp: DocumentAcp,
    block_cache_bytes: usize,
}

impl PreflightProtectedBackendStore<'_> {
    /// Size the served node's block cache; the preflight check itself
    /// always opens with the default.
    #[must_use]
    pub fn with_block_cache_bytes(mut self, block_cache_bytes: usize) -> Self {
        self.block_cache_bytes = block_cache_bytes;
        self
    }

    /// Construct the served node from the same locked data directory whose
    /// provenance was checked. Callers cannot substitute another native node.
    /// The caller reads `plugins` from its plugin store while it holds that
    /// store's lease, and keeps the lease until this returns. The node is
    /// returned already carrying `composition`, so no caller can serve
    /// requests from a node its lifecycle points cannot find.
    pub async fn start_node_with_projection(
        self,
        key: &StoreKey,
        runtime: Arc<dyn crate::identity::NodeIdentity>,
        configure: impl FnOnce(defra_node::NodeBuilder) -> defra_node::NodeBuilder + Send,
        policy: Arc<dyn super::ToolSourcePolicy>,
        plugins: Vec<super::ProjectedPlugin>,
        composition: &crate::node_extension::Extensions,
    ) -> Result<(Arc<defra_node::Node>, BackendAdmission)> {
        if runtime.did() != self.node_did {
            return Err(BackendProtectionError::WrongNode);
        }
        let mut extensions = composition.clone();
        if let Some(group) = &self.transport_replica {
            extensions.attach(Arc::new(group.clone()))?;
        }
        let started = self
            .start_node_with_plugins(key, runtime, configure, policy, plugins)
            .await?;
        if let Err(error) = crate::node_extension::attach_all(&started.0, &extensions) {
            let _ = started.0.shutdown().await;
            return Err(error.into());
        }
        Ok(started)
    }

    async fn start_node_with_plugins(
        self,
        key: &StoreKey,
        runtime: Arc<dyn crate::identity::NodeIdentity>,
        configure: impl FnOnce(defra_node::NodeBuilder) -> defra_node::NodeBuilder + Send,
        policy: Arc<dyn super::ToolSourcePolicy>,
        plugins: Vec<super::ProjectedPlugin>,
    ) -> Result<(Arc<defra_node::Node>, BackendAdmission)> {
        let projection = super::document_projector_with_installed_plugins(
            runtime.clone(),
            policy,
            plugins,
            self.transport_replica
                .as_ref()
                .map(|replica| replica.reader_manager()),
            document_acp::recorded_policy_ids(&self.store.data)?,
        )
        .map_err(|error| BackendProtectionError::Schema(error.into()))?;
        let builder =
            super::store_key::persistent_builder(&self.store.data, key, self.block_cache_bytes)
                .map_err(|error| BackendProtectionError::Schema(error.into()))?
                .with_storage_durability(self.store.durability)
                .with_document_projector(projection)
                .with_node_identity_did(self.node_did)
                .with_node_acp_enabled();
        let builder =
            document_acp::configure(builder, &self.store.data, key, self.document_acp, &runtime)?;
        let builder = configure(builder);
        let builder = match self.transport_replica {
            Some(replica) => builder.with_replica_registration(replica.registration()),
            None => builder,
        };
        let node = build_configured(builder, &self.store.data, self.document_acp).await?;
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

async fn require_device_group(
    node: &Arc<defra_node::Node>,
    device: &str,
    account: &identity::Did,
) -> Result<crate::config_client::transport_replica::TransportReplica> {
    let actor = identity::Did::new(device).map_err(|_| BackendProtectionError::WrongNode)?;
    crate::config_client::ConfigAccess::local_as(node.clone(), actor)
        .transport_replica(account)
        .await?
        .ok_or_else(|| BackendProtectionError::DeviceNotAdmitted(account.clone()))
}

/// The owner grants the Node `Identity.GrantSets.nodeRuntimePermissions` once,
/// when the fresh store is first opened and before anything else is written.
async fn grant_node_runtime(
    node: &defra_node::Node,
    owner: &identity::Did,
    node_did: &str,
) -> Result<()> {
    let context = defra_core::RequestContext::default().with_identity(Some(owner.to_string()));
    for permission in crate::identity::node_runtime_permissions() {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), node_did)
            .await?;
    }
    Ok(())
}

/// `Identity.NodeProvisioning.runtimeMayStart`: every authorization decision
/// assumes NAC is enabled and that the Node reads only what document ACP
/// grants it. Native treats every actor as admin while NAC is off, and lets
/// the NAC owner and every `bypass-dac` holder read past document ACP, so a
/// node observed either way is refused before it serves or admits work. This
/// is the one runtime check of that invariant.
async fn require_runtime_may_start(node: &defra_node::Node) -> Result<()> {
    let nac = node.nac_manager();
    if !nac.is_enabled().await {
        return Err(BackendProtectionError::NodeAcpNotEnabled {
            status: nac.info().await.status,
        });
    }
    let did = node
        .node_identity_did()
        .and_then(|did| identity::Did::new(did).ok())
        .ok_or(BackendProtectionError::InvalidProvenance)?;
    if nac
        .check_permission(&did, acp::nac::NodePermission::DacBypass)
        .await
        .map_err(BackendProtectionError::NodeAcpRead)?
    {
        return Err(BackendProtectionError::NodeReadsPastDocumentAcp);
    }
    Ok(())
}

async fn validate_binding(node: &defra_node::Node) -> Result<()> {
    require_runtime_may_start(node).await?;
    validate_policy_binding(node, document_acp::node_policy_ids(node)).await
}

/// The collection binding alone, against the home's policy `ids`, for a copy
/// of a store opened without its node's identity, which
/// [`require_runtime_may_start`] needs.
pub(crate) async fn validate_policy_binding(
    node: &defra_node::Node,
    ids: &gents_protocol::schemas::HomePolicyIds,
) -> Result<()> {
    use gents_protocol::schemas::{BuiltInPolicy, INFERENCE_BACKEND_NAME};
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
                policy.id == ids.id(BuiltInPolicy::Household)
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
