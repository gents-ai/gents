mod bootstrap;
mod enrollment;
mod error;
mod file_review;
mod goal;
mod observation_scope;
mod p2p_ops;
mod route_manager;
mod supervisor;
pub(super) mod sync_state;
mod writes;

#[cfg(test)]
mod tests;

use rapidhash::RapidHashMap;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use defra_p2p_adapter::P2POperations as P2POps;
use gents::config_client::ConfigAccess;
use gents::P2pSyncStatusSnapshot;
use gents_protocol::peer_schema::ReplicatedSchemaSkew;
use p2p::iroh::{IrohDiscoveryConfig, IrohRelayModeConfig};
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;

use super::observe::{ObservedStore, ObserverHandle};
use super::paths::DesktopPaths;
use super::peer_directory::PeerRecord;
use super::principal_identity::PrincipalIdentity;
use super::query::{
    load_full_snapshot_with_peer_records, load_node_scoped_snapshot_with_peer_records,
};
use crate::remote_admin::PairingErrorClass;

pub use enrollment::EnrollmentRequestResult;
pub(crate) use error::ClientCoreError;
pub use observation_scope::ClientObservationAttachment;
pub use sync_state::RuntimeSchemaObservation;
pub use writes::RefreshedRequestProjection;

const BOOTSTRAP_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const PEER_ADD_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_OPERATION_BACKOFF: Duration = Duration::from_millis(250);
const P2P_SUPERVISOR_INTERVAL: Duration = Duration::from_secs(2);
const P2P_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const P2P_WEDGED_FAILURE_THRESHOLD: u32 = 3;
const DESKTOP_P2P_MAX_CONCURRENT_PUSH_TASKS: usize = 32;
const DESKTOP_P2P_RATE_LIMIT_BURST: u32 = 5_000;
const DESKTOP_P2P_RATE_LIMIT_RATE: f64 = 500.0;

#[derive(Debug, Clone)]
pub struct ClientCoreOptions {
    pub port: u16,
    pub bind_addr: Option<IpAddr>,
    // Defra's HTTP listener lives until its Tokio runtime stops;
    // `EmbeddedNode::shutdown` alone does not release it.
    pub http_addr: Option<SocketAddr>,
    pub relay_mode: IrohRelayModeConfig,
    pub discovery: IrohDiscoveryConfig,
    pub load_persisted_collections: bool,
    pub max_concurrent_dag_fetches: usize,
    pub max_concurrent_push_tasks: usize,
    pub rate_limit_burst: u32,
    pub rate_limit_rate: f64,
    pub max_pending_dags: usize,
    pub install_replicators_on_bootstrap: bool,
    pub store_key_custody: gents::store_key::StoreKeyCustodyChoice,
    pub store_key_provider: Option<Arc<dyn ClientStoreKeyProvider>>,
    /// Headless observation must not open platform credential dialogs. Explicit
    /// user commands retain their existing operator authority requirements.
    pub operator_identity_interaction: crate::local_runtime::OperatorIdentityInteraction,
}

/// Only a trusted native host may supply store keys. It must bind its lookup
/// to this exact custody record. Native custody remains the durable owner;
/// a host cache must never be the only copy. Keys never enter product messages or logs.
#[async_trait::async_trait]
pub trait ClientStoreKeyProvider: std::fmt::Debug + Send + Sync {
    async fn load(
        &self,
        record: &gents::store_key::StoreEncryption,
        key_file: &std::path::Path,
        data_path: &std::path::Path,
    ) -> Result<gents::store_key::StoreKey>;
}

impl Default for ClientCoreOptions {
    fn default() -> Self {
        Self {
            port: 0,
            bind_addr: None,
            http_addr: None,
            relay_mode: IrohRelayModeConfig::default(),
            discovery: IrohDiscoveryConfig::default(),
            load_persisted_collections: false,
            max_concurrent_dag_fetches: p2p::sync::DEFAULT_MAX_CONCURRENT_DAG_FETCHES,
            max_concurrent_push_tasks: DESKTOP_P2P_MAX_CONCURRENT_PUSH_TASKS,
            rate_limit_burst: DESKTOP_P2P_RATE_LIMIT_BURST,
            rate_limit_rate: DESKTOP_P2P_RATE_LIMIT_RATE,
            max_pending_dags: p2p::sync::DEFAULT_MAX_PENDING_DAGS,
            install_replicators_on_bootstrap: true,
            store_key_custody: gents::store_key::StoreKeyCustodyChoice::default(),
            store_key_provider: None,
            operator_identity_interaction: Default::default(),
        }
    }
}

impl ClientCoreOptions {
    pub fn local_only() -> Self {
        Self {
            bind_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            relay_mode: IrohRelayModeConfig::Disabled,
            discovery: IrohDiscoveryConfig::Disabled,
            store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
            ..Self::default()
        }
    }

    #[doc(hidden)]
    pub fn local_simulated_route() -> Self {
        Self {
            install_replicators_on_bootstrap: false,
            ..Self::local_only()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPeerStatus {
    pub peer_id: String,
    pub label: String,
    pub node_did: String,
    pub addr: String,
    pub dial_succeeded: bool,
    pub last_error: Option<String>,
    pub pairing: Vec<PairingCollectionStatus>,
    pub routes: Vec<ClientRouteStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRouteStatus {
    pub route_id: String,
    pub direction: String,
    pub directory_id: String,
    pub transport_peer_id: Option<String>,
    pub address: Option<String>,
    pub template: Option<String>,
    pub desired: bool,
    pub applied: bool,
    pub live_match: bool,
    pub filter_summary: String,
    pub last_error: Option<String>,
    pub retry_count: u32,
    pub last_retry_at: Option<SystemTime>,
    pub last_retry_error_class: Option<PairingErrorClass>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCollectionStatus {
    pub collection_id: String,
    pub pairing_retry_count: u32,
    pub last_retry_at: Option<SystemTime>,
    pub last_retry_error_class: Option<PairingErrorClass>,
    pub stuck_since: Option<SystemTime>,
    first_failure_at: Option<SystemTime>,
}

pub const STUCK_THRESHOLD_ATTEMPTS: u32 = 6;
pub const STUCK_THRESHOLD_DURATION: Duration = Duration::from_secs(5 * 60);

impl PairingCollectionStatus {
    pub fn new(collection_id: impl Into<String>) -> Self {
        Self {
            collection_id: collection_id.into(),
            pairing_retry_count: 0,
            last_retry_at: None,
            last_retry_error_class: None,
            stuck_since: None,
            first_failure_at: None,
        }
    }

    pub fn record_retry(&mut self, class: PairingErrorClass) {
        let now = SystemTime::now();
        self.pairing_retry_count = self.pairing_retry_count.saturating_add(1);
        self.last_retry_at = Some(now);
        self.last_retry_error_class = Some(class);
        if self.first_failure_at.is_none() {
            self.first_failure_at = Some(now);
        }
    }

    pub fn record_success(&mut self) {
        self.pairing_retry_count = 0;
        self.last_retry_at = None;
        self.last_retry_error_class = None;
        self.stuck_since = None;
        self.first_failure_at = None;
    }

    pub fn update_stuck_indicator(&mut self, now: SystemTime) {
        if self.stuck_since.is_some() {
            return;
        }
        let attempts_trigger = self.pairing_retry_count >= STUCK_THRESHOLD_ATTEMPTS;
        let duration_trigger = self
            .first_failure_at
            .and_then(|first| now.duration_since(first).ok())
            .map(|elapsed| elapsed >= STUCK_THRESHOLD_DURATION)
            .unwrap_or(false);
        if attempts_trigger || duration_trigger {
            self.stuck_since = Some(now);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum P2PHealthStatus {
    Healthy,
    Degraded,
    Wedged,
}

impl P2PHealthStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Wedged => "wedged",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P2PHealth {
    pub status: P2PHealthStatus,
    pub consecutive_failures: u32,
    pub connected_peer_count: usize,
    pub replicator_count: usize,
    pub last_error: Option<String>,
    pub last_ok_at: Option<SystemTime>,
    pub last_failure_at: Option<SystemTime>,
}

impl Default for P2PHealth {
    fn default() -> Self {
        Self {
            status: P2PHealthStatus::Healthy,
            consecutive_failures: 0,
            connected_peer_count: 0,
            replicator_count: 0,
            last_error: None,
            last_ok_at: None,
            last_failure_at: None,
        }
    }
}

impl P2PHealth {
    pub fn status_label(&self) -> &'static str {
        self.status.label()
    }
}

pub(super) fn p2p_health_materially_changed(previous: &P2PHealth, next: &P2PHealth) -> bool {
    previous.status != next.status
        || previous.consecutive_failures != next.consecutive_failures
        || previous.connected_peer_count != next.connected_peer_count
        || previous.replicator_count != next.replicator_count
        || previous.last_error != next.last_error
}

// The transport counters are diagnostic. Product sync readiness is derived
// from the configured peer rows in this same revision, never from an
// independently sampled global connection count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSyncStateSnapshot {
    pub transport: P2PHealth,
    pub database_sync: Option<P2pSyncStatusSnapshot>,
    pub database_sync_error: Option<String>,
    // DefraDB accepts the connection but never merges either side's writes;
    // schema skew is the only visible signal.
    pub peer_schema_skew: BTreeMap<String, ReplicatedSchemaSkew>,
    // Bridge projections consume this copy instead of racing a second
    // directory read after receiving a sync update.
    pub directory: Vec<PeerRecord>,
    pub peers: Vec<ClientPeerStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum P2PSupervisorCommand {
    RepairNow,
}

/// A finished shutdown's result, with the error rendered for every waiter.
type ShutdownOutcome = std::result::Result<(), String>;

pub struct ClientCore {
    store_access: Mutex<Option<ConfigAccess>>,
    paths: DesktopPaths,
    options: ClientCoreOptions,
    node_identity: PrincipalIdentity,
    node: Arc<EmbeddedNode>,
    p2p: Arc<dyn P2POps>,
    route_manager: Arc<route_manager::ClientRouteManager>,
    store: Arc<ObservedStore>,
    observer: Mutex<Option<ObserverHandle>>,
    sync_state: sync_state::ClientSyncStateOwner,
    p2p_supervisor: Mutex<Option<JoinHandle<()>>>,
    observation_scope: observation_scope::ObservationScope,
    request_patch_signatures: tokio::sync::Mutex<RapidHashMap<String, (usize, usize, u64)>>,
    p2p_control: Mutex<Option<mpsc::Sender<P2PSupervisorCommand>>>,
    shutdown_completion: Mutex<Option<watch::Receiver<Option<ShutdownOutcome>>>>,
    #[cfg(test)]
    shutdown_test_pause: Mutex<Option<Arc<ShutdownTestPause>>>,
    last_mutation_error: StdRwLock<Option<String>>,
    local_peer_id: String,
    listen_addresses: Vec<String>,
    bootstrap_errors: Vec<String>,
}

#[cfg(test)]
struct ShutdownTestPause {
    node_shutdown_started: tokio::sync::Notify,
    allow_node_shutdown: tokio::sync::Notify,
    node_shutdown_returned: tokio::sync::Notify,
    allow_access_release: tokio::sync::Notify,
}

impl ClientCore {
    pub fn paths(&self) -> &DesktopPaths {
        &self.paths
    }

    pub fn options(&self) -> &ClientCoreOptions {
        &self.options
    }

    pub fn node_identity(&self) -> &PrincipalIdentity {
        &self.node_identity
    }

    pub fn node(&self) -> &EmbeddedNode {
        self.node.as_ref()
    }

    pub fn node_arc(&self) -> Arc<EmbeddedNode> {
        Arc::clone(&self.node)
    }

    pub fn p2p(&self) -> &Arc<dyn P2POps> {
        &self.p2p
    }

    #[doc(hidden)]
    pub async fn add_local_standard_peer_for_test(&self, node_did: &str) -> Result<()> {
        if self.options.install_replicators_on_bootstrap {
            return Err(ClientCoreError::SimulatedRouteRequiresLocalOptions.into());
        }
        self.add_local_standard_peer_route_for_test(
            "Test Local Runtime",
            "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
            node_did,
            "http://127.0.0.1:56001/graphql",
            "/tmp/test-agent-home",
        )
        .await
    }

    #[doc(hidden)]
    pub async fn add_local_standard_peer_route_for_test(
        &self,
        label: &str,
        addr: &str,
        node_did: &str,
        graphql: &str,
        agent_home: &str,
    ) -> Result<()> {
        let record = self
            .sync_state
            .upsert_local_standard_peer(label, addr, node_did, graphql, agent_home)
            .await?;
        self.sync_state.set_pairing_ready(&record, true).await?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    pub async fn add_managed_enrollment_peer_for_test(
        &self,
        node_did: &str,
        graphql: &str,
        agent_home: &str,
        authorization_sequence: u64,
    ) -> Result<()> {
        let addr =
            "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
        self.sync_state
            .upsert_local_standard_peer("Managed Runtime", addr, node_did, graphql, agent_home)
            .await?;
        let record = self
            .sync_state
            .upsert_enrollment_peer(
                "managed-runtime-peer",
                "Managed Runtime",
                addr,
                node_did,
                "network",
                "request",
                "digest",
                node_did,
                authorization_sequence,
                "2999-01-01T00:00:00Z",
            )
            .await?;
        self.sync_state.set_pairing_ready(&record, true).await?;
        Ok(())
    }

    pub fn local_peer_id(&self) -> &str {
        &self.local_peer_id
    }

    pub fn listen_addresses(&self) -> &[String] {
        &self.listen_addresses
    }

    pub fn sync_state(&self) -> ClientSyncStateSnapshot {
        self.sync_state.snapshot()
    }

    pub fn sync_state_updates(&self) -> watch::Receiver<ClientSyncStateSnapshot> {
        self.sync_state.subscribe()
    }

    // The signed enrollment remains durable; the replacement process must
    // re-establish both route legs before chat writes reopen.
    pub async fn mark_managed_runtime_restarting(&self, node_did: &str) -> Result<()> {
        let records = self.peer_records().await;
        for record in records.into_iter().filter(|record| {
            record.node_did == node_did
                && record.is_enrollment()
                && record.is_managed_runtime()
                && record.pairing_ready
        }) {
            self.sync_state.set_pairing_ready(&record, false).await?;
        }
        Ok(())
    }

    async fn send_p2p_command(&self, command: P2PSupervisorCommand) -> Result<(), ClientCoreError> {
        let sender = self
            .p2p_control
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or(ClientCoreError::P2pSupervisorShuttingDown)?;
        sender
            .send(command)
            .await
            .map_err(|source| ClientCoreError::QueueP2pSupervisorCommand { source })
    }

    pub fn set_selected_node_did(&self, node_did: Option<String>) {
        self.observation_scope.select(node_did);
    }

    pub fn selected_node_did(&self) -> Option<String> {
        self.observation_scope.current()
    }

    pub fn selected_node_did_rx(&self) -> watch::Receiver<Option<String>> {
        self.observation_scope.subscribe()
    }

    pub fn observe_all_nodes(&self) -> Result<ClientObservationAttachment> {
        Ok(self.observation_scope.attach()?)
    }

    pub async fn peer_records(&self) -> Vec<PeerRecord> {
        self.sync_state.records()
    }

    pub fn store(&self) -> &Arc<ObservedStore> {
        &self.store
    }

    pub fn store_updates(&self) -> watch::Receiver<u64> {
        self.store.subscribe()
    }

    pub fn store_change_updates(&self) -> watch::Receiver<super::StoreUpdateNotice> {
        self.store.subscribe_changes()
    }

    pub fn bootstrap_errors(&self) -> &[String] {
        &self.bootstrap_errors
    }

    pub fn last_mutation_error(&self) -> Option<String> {
        self.last_mutation_error
            .read()
            .expect("mutation error lock poisoned")
            .clone()
    }

    pub async fn request_p2p_repair(&self) -> Result<()> {
        Ok(self
            .send_p2p_command(P2PSupervisorCommand::RepairNow)
            .await?)
    }

    pub async fn refresh_store(&self) -> Result<u64> {
        let scoped = self.selected_node_did();
        let captured = self.store.capture_read();
        let records = self.sync_state.records();
        let snapshot = match scoped.as_deref() {
            Some(did) => {
                load_node_scoped_snapshot_with_peer_records(
                    self.node.as_ref(),
                    did,
                    &records,
                    self.node_identity.did(),
                    self.options.operator_identity_interaction,
                )
                .await?
            }
            None => {
                load_full_snapshot_with_peer_records(
                    self.node.as_ref(),
                    &records,
                    self.node_identity.did(),
                    self.options.operator_identity_interaction,
                )
                .await?
            }
        };
        let rows = snapshot.row_count();
        let published =
            self.store
                .replace_reloaded_snapshot_at_read(&captured, scoped.as_deref(), snapshot);
        if !published {
            self.store.request_resync(scoped.as_deref());
        }
        let version = self.store.projection_revision().store_version;
        tracing::debug!(
            target: "gents_desktop_core::replication",
            version,
            rows,
            published,
            scoped = scoped.is_some(),
            "desktop local replica snapshot refreshed"
        );
        Ok(version)
    }

    pub async fn refresh_node(&self, node_did: &str) -> Result<Option<u64>> {
        let node_did = node_did.trim();
        if node_did.is_empty() {
            return Ok(None);
        }
        let captured = self.store.capture_read();
        let records = self.sync_state.records();
        let snapshot = match load_node_scoped_snapshot_with_peer_records(
            self.node.as_ref(),
            node_did,
            &records,
            self.node_identity.did(),
            self.options.operator_identity_interaction,
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.store.request_resync(Some(node_did));
                return Err(error.into());
            }
        };
        let rows = snapshot.row_count();
        let published =
            self.store
                .replace_reloaded_snapshot_at_read(&captured, Some(node_did), snapshot);
        if !published {
            self.store.request_resync(Some(node_did));
            return Ok(None);
        }
        let version = self.store.projection_revision().store_version;
        tracing::debug!(
            target: "gents_desktop_core::replication",
            node_did,
            version,
            rows,
            published,
            "desktop refreshed agent projection from local replica"
        );
        Ok(Some(version))
    }

    pub async fn observer_metrics(
        &self,
    ) -> Option<crate::client::observe::ObserverMetricsSnapshot> {
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
        tracing::info!("client core shutdown: begin");
        self.p2p_control.lock().await.take();
        if let Some(task) = self.p2p_supervisor.lock().await.take() {
            tracing::info!("client core shutdown: stopping p2p supervisor");
            task.abort();
            let _ = task.await;
        }
        if let Some(observer) = self.observer.lock().await.take() {
            tracing::info!("client core shutdown: stopping observer");
            observer.shutdown().await;
        }
        tracing::info!("client core shutdown: stopping embedded node");
        #[cfg(test)]
        if let Some(pause) = self.shutdown_test_pause.lock().await.clone() {
            pause.node_shutdown_started.notify_one();
            pause.allow_node_shutdown.notified().await;
        }
        self.node.shutdown().await;
        #[cfg(test)]
        if let Some(pause) = self.shutdown_test_pause.lock().await.clone() {
            pause.node_shutdown_returned.notify_one();
            pause.allow_access_release.notified().await;
        }
        self.store_access.lock().await.take();
        tracing::info!("client core shutdown: complete");
        Ok(())
    }
}
