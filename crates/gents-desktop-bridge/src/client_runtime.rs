mod access;
mod authored_delivery;
pub use authored_delivery::SavedForDelivery;
mod bootstrap;
mod context_details;
mod file_inventory;
mod file_review;
mod goal;
mod inference_activity;
mod inference_observation;
mod inference_setup;
mod owed_settlement;
mod writes;

use anyhow::{Context, Result};
use gents_desktop_core::client::journal::ClientJournal;
use gents_desktop_core::client::journal::ClientStoreKeyProvider;
use gents_observe::client_projection::{
    observe::{ObservedStores, ObserverHandle},
    query::load_node_scoped_snapshot_with_connections,
};
use gents_protocol::runtime_connection::RuntimeConnection;
use gents_store::client_journal::paths::DesktopPaths;
use gents_store::client_journal::principal_identity::PrincipalIdentity;
use gents_store::client_journal::runtime_directory::RuntimeDirectory;
use rapidhash::RapidHashMap;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::{watch, Mutex};

pub use goal::RetainedGoalSubmission;
pub use inference_observation::{InferenceActivity, InferenceObservation, InferenceTarget};
pub use writes::{ChatRouteRefusal, LocalHomeAccessError, RefreshedRequestProjection};

#[derive(Debug, Clone, Default)]
pub struct ClientRuntimeOptions {
    pub store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice,
    pub store_key_provider: Option<Arc<dyn ClientStoreKeyProvider>>,
}

/// How a direct refresh read settled (`ObserverResync.Settlement`). Both
/// carry the node's store version after the refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreRefresh {
    /// The read replaced the node's store.
    Published { version: u64 },
    /// A newer publication into the node's store rejected the read; the
    /// observer was asked to read the node again and owns its catch-up.
    Superseded { version: u64 },
}

impl StoreRefresh {
    fn at(published: bool, version: u64) -> Self {
        if published {
            Self::Published { version }
        } else {
            Self::Superseded { version }
        }
    }

    pub fn version(self) -> u64 {
        match self {
            Self::Published { version } | Self::Superseded { version } => version,
        }
    }
}

type ShutdownOutcome = std::result::Result<(), String>;

pub struct ClientRuntime {
    pub(crate) owed: owed_settlement::OwedSettlements,
    delivery_started: std::sync::atomic::AtomicBool,
    delivery_cancel: tokio_util::sync::CancellationToken,
    delivery_lock: Mutex<()>,
    access_requests: Mutex<()>,
    runtime_endpoints: Arc<gents_store::config_client::runtime_endpoints::RuntimeEndpoints>,
    journal: Arc<ClientJournal>,
    paths: DesktopPaths,
    options: ClientRuntimeOptions,
    node_identity: PrincipalIdentity,
    node: Arc<defra_node::Node>,
    directory: RuntimeDirectory,
    stores: Arc<ObservedStores>,
    observer: Mutex<Option<ObserverHandle>>,
    inference_observation_epoch: std::sync::atomic::AtomicU64,
    inference_observations: std::sync::Mutex<
        RapidHashMap<(String, String), inference_observation::InferenceObservationOwner>,
    >,
    request_patch_signatures: Mutex<RapidHashMap<String, (usize, usize, u64)>>,
    context_details: context_details::ContextDetailsCache,
    shutdown_completion: Mutex<Option<watch::Receiver<Option<ShutdownOutcome>>>>,
    last_mutation_error: StdRwLock<Option<String>>,
}

impl ClientRuntime {
    pub fn store_release(&self) -> gents_store::local::home::StoreRelease {
        self.journal.store_release()
    }

    pub fn paths(&self) -> &DesktopPaths {
        &self.paths
    }
    pub fn options(&self) -> &ClientRuntimeOptions {
        &self.options
    }
    pub fn node_identity(&self) -> &PrincipalIdentity {
        &self.node_identity
    }
    pub fn node(&self) -> &defra_node::Node {
        &self.node
    }
    pub fn node_arc(&self) -> Arc<defra_node::Node> {
        self.node.clone()
    }
    pub fn runtime_connections(&self) -> Vec<RuntimeConnection> {
        self.directory.records()
    }
    pub fn runtime_connection_updates(&self) -> watch::Receiver<Vec<RuntimeConnection>> {
        self.directory.subscribe()
    }
    pub fn stores(&self) -> &Arc<ObservedStores> {
        &self.stores
    }
    pub fn store_updates(&self) -> watch::Receiver<u64> {
        self.stores.subscribe()
    }
    pub fn last_mutation_error(&self) -> Option<String> {
        self.last_mutation_error
            .read()
            .expect("mutation error lock poisoned")
            .clone()
    }

    #[cfg(feature = "test-fixtures")]
    pub async fn stop_observer_for_test(&self) {
        if let Some(observer) = self.observer.lock().await.take() {
            observer.shutdown().await;
        }
    }

    pub async fn mark_managed_runtime_restarting(&self, node_did: &str) -> Result<()> {
        self.request_runtime_reconnect(Some(node_did)).await;
        Ok(())
    }

    pub async fn reconnect_runtime_now(
        &self,
        node_did: &str,
    ) -> Option<gents_observe::client_projection::observe::RuntimeConnectionStatus> {
        let settled = self.observer.lock().await.as_ref()?.retry_now(node_did);
        settled.await
    }

    pub async fn request_runtime_reconnect(&self, node_did: Option<&str>) {
        if let Some(observer) = self.observer.lock().await.as_ref() {
            match node_did {
                Some(node_did) => drop(observer.retry_now(node_did)),
                None => observer.retry_all_now(),
            }
        }
    }

    /// Reads `node_did` through its own connection and publishes the read
    /// into its own store through the store fence. A read superseded by a
    /// newer publication is handed to the observer, which reads the node again.
    pub async fn refresh_node(&self, node_did: &str) -> Result<StoreRefresh> {
        let node_did = node_did.trim();
        anyhow::ensure!(!node_did.is_empty(), "a refresh names its node");
        let store = self.stores.ensure(node_did);
        let captured = store.capture_read();
        let records = self.directory.records();
        let snapshot = match load_node_scoped_snapshot_with_connections(
            node_did,
            &records,
            &self.runtime_endpoints,
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.observe_refresh_failure(node_did, &error);
                return Err(error.into());
            }
        };
        let rows = snapshot.row_count();
        let published = store.replace_reloaded_snapshot_at_read(&captured, snapshot);
        if !published {
            store.request_resync();
        }
        let refresh = StoreRefresh::at(published, store.projection_revision().store_version);
        tracing::debug!(
            target: "gents_desktop_core::replication",
            node_did,
            ?refresh,
            rows,
            "desktop refreshed node projection from its runtime"
        );
        Ok(refresh)
    }

    /// The observer owns runtime connection state: a refusal clears the
    /// node's rows and asks it to confirm with its own authenticated read.
    fn observe_refresh_failure(
        &self,
        node_did: &str,
        error: &gents_observe::client_projection::query::SnapshotReadError,
    ) {
        let store = self.stores.ensure(node_did);
        if error.refused() {
            store.clear_after_refused_read();
        } else {
            store.request_resync();
        }
    }

    pub async fn observer_metrics(
        &self,
    ) -> Option<gents_observe::client_projection::observe::ObserverMetricsSnapshot> {
        self.observer
            .lock()
            .await
            .as_ref()
            .map(|h| h.metrics_snapshot())
    }

    pub async fn shutdown(self: &Arc<Self>) -> Result<()> {
        let mut completion = self.shutdown_completion.lock().await;
        if completion.is_none() {
            let (result_tx, result_rx) = watch::channel(None);
            *completion = Some(result_rx.clone());
            let owner = Arc::clone(self);
            tokio::spawn(async move {
                let result = owner
                    .shutdown_inner()
                    .await
                    .map_err(|error| error.to_string());
                result_tx.send_replace(Some(result));
            });
        }
        let mut result_rx = completion
            .as_ref()
            .expect("shutdown owner installed")
            .clone();
        drop(completion);

        loop {
            if let Some(result) = result_rx.borrow().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            result_rx
                .changed()
                .await
                .context("client shutdown owner ended without a result")?;
        }
    }

    async fn shutdown_inner(&self) -> Result<()> {
        self.delivery_cancel.cancel();
        let _delivery = self.delivery_lock.lock().await;
        if let Some(observer) = self.observer.lock().await.take() {
            observer.shutdown().await;
        }
        self.owed.stop().await;
        self.journal.shutdown().await?;
        Ok(())
    }

    pub fn session_unreadable_reason(&self, node_did: &str) -> Option<&'static str> {
        self.operator_observation_graphql(node_did)
            .is_none()
            .then_some("Connect to the runtime to read this session.")
    }

    pub async fn reload_session(&self, session_id: &str, node_did: &str) -> Result<()> {
        self.refresh_local_request(node_did, session_id, None, None, &[])
            .await?;
        Ok(())
    }
}
