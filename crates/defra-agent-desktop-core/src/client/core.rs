mod bootstrap;
mod materialization;
mod p2p_ops;
mod supervisor;
mod writes;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use defra_p2p_adapter::P2POperations as P2POps;
use p2p::iroh::{IrohDiscoveryConfig, IrohRelayModeConfig};
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tokio::task::JoinHandle;

use super::observe::{ObservedStore, ObserverHandle};
use super::paths::DesktopPaths;
use super::peer_directory::{PeerDirectory, PeerRecord};
use super::principal_identity::PrincipalIdentity;
use super::query::{
    load_agent_scoped_snapshot, load_full_snapshot, load_full_snapshot_from_graphql,
};

const BOOTSTRAP_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const PEER_ADD_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_OPERATION_BACKOFF: Duration = Duration::from_millis(250);
const P2P_SUPERVISOR_INTERVAL: Duration = Duration::from_secs(2);
// Live tool-heavy turns can keep the embedded transport busy long enough that
// short probe deadlines misclassify healthy peers as wedged.
const P2P_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const P2P_WEDGED_FAILURE_THRESHOLD: u32 = 3;
const DESKTOP_P2P_MAX_CONCURRENT_PUSH_TASKS: usize = 32;
const DESKTOP_P2P_RATE_LIMIT_BURST: u32 = 5_000;
const DESKTOP_P2P_RATE_LIMIT_RATE: f64 = 500.0;

#[derive(Debug, Clone)]
pub struct ClientCoreOptions {
    pub port: u16,
    pub bind_addr: Option<IpAddr>,
    pub relay_mode: IrohRelayModeConfig,
    pub discovery: IrohDiscoveryConfig,
    pub load_persisted_collections: bool,
    pub max_concurrent_dag_fetches: usize,
    pub max_concurrent_push_tasks: usize,
    pub rate_limit_burst: u32,
    pub rate_limit_rate: f64,
    pub install_replicators_on_bootstrap: bool,
}

impl Default for ClientCoreOptions {
    fn default() -> Self {
        Self {
            port: 0,
            bind_addr: None,
            relay_mode: IrohRelayModeConfig::default(),
            discovery: IrohDiscoveryConfig::default(),
            load_persisted_collections: false,
            max_concurrent_dag_fetches: 4,
            max_concurrent_push_tasks: DESKTOP_P2P_MAX_CONCURRENT_PUSH_TASKS,
            rate_limit_burst: DESKTOP_P2P_RATE_LIMIT_BURST,
            rate_limit_rate: DESKTOP_P2P_RATE_LIMIT_RATE,
            install_replicators_on_bootstrap: true,
        }
    }
}

impl ClientCoreOptions {
    pub fn local_only() -> Self {
        Self {
            bind_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            relay_mode: IrohRelayModeConfig::Disabled,
            discovery: IrohDiscoveryConfig::Disabled,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientPeerStatus {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub addr: String,
    pub dial_succeeded: bool,
    pub last_error: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum P2PSupervisorCommand {
    RepairNow,
}

pub struct ClientCore {
    paths: DesktopPaths,
    options: ClientCoreOptions,
    principal: PrincipalIdentity,
    node: Arc<EmbeddedNode>,
    p2p: Arc<dyn P2POps>,
    peer_directory: Arc<RwLock<PeerDirectory>>,
    store: Arc<ObservedStore>,
    observer: Mutex<Option<ObserverHandle>>,
    peer_statuses: Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    p2p_supervisor: Mutex<Option<JoinHandle<()>>>,
    materialization_supervisor: Mutex<Option<JoinHandle<()>>>,
    p2p_health: watch::Sender<P2PHealth>,
    selected_agent_did: watch::Sender<Option<String>>,
    last_loaded_for: tokio::sync::Mutex<HashMap<String, std::time::Instant>>,
    p2p_control: Mutex<Option<mpsc::Sender<P2PSupervisorCommand>>>,
    last_mutation_error: StdRwLock<Option<String>>,
    local_peer_id: String,
    listen_addresses: Vec<String>,
    bootstrap_errors: Vec<String>,
}

impl ClientCore {
    pub fn paths(&self) -> &DesktopPaths {
        &self.paths
    }

    pub fn options(&self) -> &ClientCoreOptions {
        &self.options
    }

    pub fn principal(&self) -> &PrincipalIdentity {
        &self.principal
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

    pub fn local_peer_id(&self) -> &str {
        &self.local_peer_id
    }

    pub fn listen_addresses(&self) -> &[String] {
        &self.listen_addresses
    }

    pub fn peer_statuses(&self) -> Vec<ClientPeerStatus> {
        self.peer_statuses
            .read()
            .expect("peer status lock poisoned")
            .clone()
    }

    pub fn p2p_health(&self) -> P2PHealth {
        self.p2p_health.borrow().clone()
    }

    pub fn p2p_health_updates(&self) -> watch::Receiver<P2PHealth> {
        self.p2p_health.subscribe()
    }

    pub fn set_selected_agent_did(&self, agent_did: Option<String>) {
        self.selected_agent_did.send_replace(agent_did);
    }

    pub fn selected_agent_did(&self) -> Option<String> {
        self.selected_agent_did.borrow().clone()
    }

    pub fn selected_agent_did_rx(&self) -> watch::Receiver<Option<String>> {
        self.selected_agent_did.subscribe()
    }

    pub async fn peer_records(&self) -> Vec<PeerRecord> {
        self.peer_directory.read().await.records().to_vec()
    }

    pub fn store(&self) -> &Arc<ObservedStore> {
        &self.store
    }

    pub fn store_updates(&self) -> watch::Receiver<u64> {
        self.store.subscribe()
    }

    pub fn bootstrap_errors(&self) -> &[String] {
        &self.bootstrap_errors
    }

    pub fn peer_issue_count(&self) -> usize {
        self.peer_statuses
            .read()
            .expect("peer status lock poisoned")
            .iter()
            .filter(|status| status.last_error.is_some())
            .count()
    }

    pub fn configured_peer_count(&self) -> usize {
        self.peer_statuses
            .read()
            .expect("peer status lock poisoned")
            .len()
    }

    pub fn dialed_peer_count(&self) -> usize {
        self.peer_statuses
            .read()
            .expect("peer status lock poisoned")
            .iter()
            .filter(|status| status.dial_succeeded)
            .count()
    }

    pub fn last_mutation_error(&self) -> Option<String> {
        self.last_mutation_error
            .read()
            .expect("mutation error lock poisoned")
            .clone()
    }

    pub async fn request_p2p_repair(&self) -> Result<()> {
        let sender = self
            .p2p_control
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("desktop P2P supervisor is shutting down"))?;
        sender
            .send(P2PSupervisorCommand::RepairNow)
            .await
            .context("queueing desktop P2P repair request")
    }

    pub async fn refresh_store(&self) -> Result<u64> {
        let scoped = self.selected_agent_did();
        let snapshot = match scoped.as_deref() {
            Some(did) => load_agent_scoped_snapshot(self.node.as_ref(), did).await?,
            None => load_full_snapshot(self.node.as_ref()).await?,
        };
        let rows = snapshot.row_count();
        let version = self.store.merge_snapshot(snapshot);
        tracing::debug!(
            target: "defra_agent_desktop_core::replication",
            version,
            rows,
            scoped = scoped.is_some(),
            "desktop local replica snapshot refreshed"
        );
        Ok(version)
    }

    pub async fn refresh_remote_agent(&self, agent_did: &str) -> Result<Option<u64>> {
        let agent_did = agent_did.trim();
        if agent_did.is_empty() {
            return Ok(None);
        }

        let record = {
            let peer_directory = self.peer_directory.read().await;
            peer_directory
                .records()
                .iter()
                .find(|record| record.agent_did == agent_did)
                .cloned()
        };
        let Some(record) = record else {
            return Ok(None);
        };

        self.refresh_remote_peer_record(&record).await.map(Some)
    }

    pub async fn refresh_remote_peer_record(&self, record: &PeerRecord) -> Result<u64> {
        let graphql = record
            .graphql
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("peer {} has no GraphQL endpoint", record.label))?;

        // Remote peers serve only their own agent's data, so the remote-side
        // "full snapshot" is already agent-scoped from our perspective. The
        // local-side merge is unchanged.
        let mut snapshot = load_full_snapshot_from_graphql(graphql).await?;
        snapshot.stamp_source_agent_did(&record.agent_did);
        let rows = snapshot.row_count();
        let version = self.store.merge_snapshot(snapshot);
        tracing::info!(
            target: "defra_agent_desktop_core::replication",
            peer_id = %record.peer_id,
            label = %record.label,
            agent_did = %record.agent_did,
            graphql,
            version,
            rows,
            "desktop remote peer snapshot merged (peer is single-agent scoped)"
        );
        Ok(version)
    }

    const SELECTION_RELOAD_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

    /// Ensure the local store has fresh rows for `agent_did`. If a prior
    /// `ensure_agent_loaded(agent_did)` ran within `SELECTION_RELOAD_DEBOUNCE`,
    /// returns `Ok(false)` without doing work. Otherwise loads a scoped
    /// snapshot, merges it into the store, and records the timestamp.
    pub async fn ensure_agent_loaded(&self, agent_did: &str) -> Result<bool> {
        let now = std::time::Instant::now();
        let mut map = self.last_loaded_for.lock().await;
        if let Some(last) = map.get(agent_did) {
            if now.duration_since(*last) < Self::SELECTION_RELOAD_DEBOUNCE {
                return Ok(false);
            }
        }
        let snapshot = load_agent_scoped_snapshot(self.node.as_ref(), agent_did).await?;
        let rows = snapshot.row_count();
        let version = self.store.merge_snapshot(snapshot);
        map.insert(agent_did.to_string(), now);
        tracing::info!(
            target: "defra_agent_desktop_core::replication",
            agent_did,
            rows,
            version,
            "ensure_agent_loaded merged scoped snapshot"
        );
        Ok(true)
    }

    /// Snapshot the observer's metrics if the observer is running.
    pub async fn observer_metrics(
        &self,
    ) -> Option<crate::client::observe::ObserverMetricsSnapshot> {
        self.observer
            .lock()
            .await
            .as_ref()
            .map(|h| h.metrics_snapshot())
    }

    pub async fn shutdown(&self) -> Result<()> {
        tracing::info!("client core shutdown: begin");
        self.p2p_control.lock().await.take();
        if let Some(task) = self.p2p_supervisor.lock().await.take() {
            tracing::info!("client core shutdown: stopping p2p supervisor");
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.materialization_supervisor.lock().await.take() {
            tracing::info!("client core shutdown: stopping materialization supervisor");
            task.abort();
            let _ = task.await;
        }
        if let Some(observer) = self.observer.lock().await.take() {
            tracing::info!("client core shutdown: stopping observer");
            observer.shutdown().await;
        }
        tracing::info!("client core shutdown: stopping embedded node");
        self.node.shutdown().await;
        tracing::info!("client core shutdown: complete");
        Ok(())
    }
}
