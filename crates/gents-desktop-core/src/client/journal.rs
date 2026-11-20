use std::sync::Arc;

use anyhow::Result;
use gents_store::config_client::ConfigAccess;
use tokio::sync::Mutex;

use gents_store::client_journal::paths::DesktopPaths;
use gents_store::client_journal::principal_identity::PrincipalIdentity;

pub mod keys;

pub const CLIENT_START_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Debug, Default)]
pub struct JournalOptions {
    pub store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice,
    pub store_key_provider: Option<Arc<dyn ClientStoreKeyProvider>>,
}

/// Only a trusted native host may supply store keys. It must bind its lookup
/// to this exact custody record. Native custody remains the durable owner;
/// a host cache must never be the only copy. Keys never enter product messages or logs.
#[async_trait::async_trait]
pub trait ClientStoreKeyProvider: std::fmt::Debug + Send + Sync {
    /// Bootstrap invokes creation only while holding the store lock, after
    /// preparing new custody for a home without metadata or an existing store.
    /// Retained stores and pending upgrades always use `load`.
    async fn create(
        &self,
        record: &gents_store::local::store_key::StoreEncryption,
        key_file: &std::path::Path,
        data_path: &std::path::Path,
    ) -> Result<gents_store::local::store_key::StoreKey>;

    async fn load(
        &self,
        record: &gents_store::local::store_key::StoreEncryption,
        key_file: &std::path::Path,
        data_path: &std::path::Path,
    ) -> Result<gents_store::local::store_key::StoreKey>;
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("preparing the encrypted client journal")]
    Prepare(#[source] anyhow::Error),
    #[error("opening the client journal store")]
    Open(#[from] defra_node::Error),
    #[error(fmt = gents_protocol::error_chain::held)]
    Schemas(#[from] gents_store::schema::journal::JournalSchemaError),
    #[error(fmt = gents_protocol::error_chain::held)]
    Access(#[from] gents_store::config_client::ConfigClientError),
}

pub struct ClientJournal {
    paths: DesktopPaths,
    identity: PrincipalIdentity,
    node: Arc<defra_node::Node>,
    access: Mutex<Option<ConfigAccess>>,
    store_release: gents_store::local::home::StoreRelease,
}

impl ClientJournal {
    pub async fn open(
        paths: DesktopPaths,
        options: JournalOptions,
    ) -> std::result::Result<Arc<Self>, JournalError> {
        let prepared: Result<_> = async {
            paths.ensure_root_dirs().await?;
            let owned = paths.clone();
            tokio::task::spawn_blocking(move || {
                gents_store::local::storage_backend::reject_legacy_store(owned.node_data_dir())
            })
            .await??;
            let lease = gents_store::local::home::acquire_store(
                paths.root(),
                paths.node_data_dir(),
                CLIENT_START_STALL_TIMEOUT,
            )
            .await?;
            let key = keys::open_or_create_client_store_key(&paths, &options).await?;
            let identity = PrincipalIdentity::load_or_create(&paths).await?;
            let data = paths.node_data_dir().to_owned();
            let signer = identity.private_key_bytes().to_vec();
            let builder = tokio::task::spawn_blocking(move || {
                gents_store::local::store_key::persistent_builder(
                    &data,
                    &key,
                    gents_store::local::storage_backend::DEFAULT_BLOCK_CACHE_BYTES,
                )
                .map(|builder| builder.with_signing_key(defra_node::SigningKey::Ed25519(signer)))
            })
            .await??;
            Ok((lease, identity, builder))
        }
        .await;
        let (lease, identity, builder) = prepared.map_err(JournalError::Prepare)?;
        let store_release = lease.release_notice();
        let node = Arc::new(builder.build().await?);
        if node.node_identity_did() != Some(identity.did()) {
            let stopped = node.shutdown().await;
            drop(lease);
            stopped?;
            return Err(JournalError::Prepare(anyhow::anyhow!(
                "client journal signer differs from its held key"
            )));
        }
        let access = match ConfigAccess::claimed_local(node.clone(), lease) {
            Ok(access) => access,
            Err(error) => {
                node.shutdown().await?;
                return Err(error.into());
            }
        };
        match gents_store::schema::journal::ensure_journal_schemas(node.clone()).await {
            Ok(()) => Ok(Arc::new(Self {
                paths,
                identity,
                node,
                access: Mutex::new(Some(access)),
                store_release,
            })),
            Err(error) => {
                if let Err(stopped) = node.shutdown().await {
                    tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&stopped), "client journal shutdown failed after opening was refused");
                }
                drop(access);
                Err(error.into())
            }
        }
    }

    pub fn paths(&self) -> &DesktopPaths {
        &self.paths
    }
    pub fn identity(&self) -> &PrincipalIdentity {
        &self.identity
    }
    pub fn node(&self) -> &Arc<defra_node::Node> {
        &self.node
    }

    pub fn store_release(&self) -> gents_store::local::home::StoreRelease {
        self.store_release.clone()
    }

    pub async fn shutdown(&self) -> std::result::Result<(), JournalError> {
        drop(
            self.access
                .lock()
                .await
                .as_ref()
                .and_then(ConfigAccess::begin_store_release),
        );
        let stopped = self.node.shutdown().await;
        if let Some(access) = self.access.lock().await.take() {
            access.release_store();
        }
        stopped?;
        Ok(())
    }
}

#[cfg(test)]
mod native;
