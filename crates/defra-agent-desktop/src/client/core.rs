use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentRequestRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow,
    ToolSelectionRow,
};
use defra_node::{EmbeddedNode, NodeBuilder, P2PConfig, StorageBackend};
use defra_p2p_adapter::P2POperations as P2POps;
use p2p::iroh::{parse_public_peer_addr, IrohDiscoveryConfig, IrohRelayModeConfig};
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Instant, MissedTickBehavior};

use super::mutations::{self, CreatedConversation, PeerMutationResult, SubmittedRequest};
use super::observe::{spawn_observer, ObservedStore, ObserverHandle};
use super::paths::DesktopPaths;
use super::peer_directory::{PeerDirectory, PeerRecord};
use super::principal_identity::PrincipalIdentity;
use super::query::load_full_snapshot;
use super::schema::{
    ensure_runtime_schemas, subscribe_all_collections, subscribed_collection_names,
};
use crate::local_runtime;

const BOOTSTRAP_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const PEER_ADD_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_OPERATION_BACKOFF: Duration = Duration::from_millis(250);
const P2P_SUPERVISOR_INTERVAL: Duration = Duration::from_secs(2);
const P2P_OPERATION_TIMEOUT: Duration = Duration::from_secs(3);
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
    p2p_health: watch::Sender<P2PHealth>,
    p2p_control: mpsc::Sender<P2PSupervisorCommand>,
    last_mutation_error: StdRwLock<Option<String>>,
    local_peer_id: String,
    listen_addresses: Vec<String>,
    bootstrap_errors: Vec<String>,
}

impl ClientCore {
    pub async fn start() -> Result<Self> {
        let paths = DesktopPaths::discover()?;
        Self::start_with_paths(paths).await
    }

    pub async fn start_with_paths(paths: DesktopPaths) -> Result<Self> {
        Self::start_with_paths_and_options(paths, ClientCoreOptions::default()).await
    }

    pub async fn start_with_paths_and_options(
        paths: DesktopPaths,
        options: ClientCoreOptions,
    ) -> Result<Self> {
        paths.ensure_root_dirs().await?;

        let principal = PrincipalIdentity::load_or_create(&paths).await?;
        let bootstrap_errors = Vec::new();
        let node = Arc::new(
            NodeBuilder::default()
                .data_path(paths.node_data_dir())
                .with_storage_backend(StorageBackend::RocksDb)
                .with_p2p(P2PConfig {
                    port: options.port,
                    bind_addr: options.bind_addr,
                    relay_mode: options.relay_mode.clone(),
                    discovery: options.discovery.clone(),
                    secret_key_path: Some(paths.iroh_secret_key_path().to_path_buf()),
                    load_persisted_collections: options.load_persisted_collections,
                    max_concurrent_dag_fetches: options.max_concurrent_dag_fetches,
                    max_concurrent_push_tasks: options.max_concurrent_push_tasks,
                    rate_limit_burst: options.rate_limit_burst,
                    rate_limit_rate: options.rate_limit_rate,
                })
                .build()
                .await
                .context("starting embedded desktop node")?,
        );

        ensure_runtime_schemas(node.as_ref()).await?;
        subscribe_all_collections(node.as_ref()).await?;
        let initial_snapshot = load_full_snapshot(node.as_ref()).await?;
        let (store, _store_updates) = ObservedStore::new(initial_snapshot);
        let observer = spawn_observer(Arc::clone(&node), Arc::clone(&store));

        let peer_directory = Arc::new(RwLock::new(
            PeerDirectory::load(paths.peer_directory_path()).await?,
        ));
        let p2p = node
            .p2p_arc()
            .context("desktop node started without P2P support")?;
        let local_peer_id = p2p_local_peer_id(&p2p)
            .await
            .context("reading desktop P2P peer id")?;
        let listen_addresses = p2p_listen_addresses(&p2p)
            .await
            .context("reading desktop P2P listen addresses")?;

        let (peer_statuses, _peer_errors) = {
            let records = peer_directory.read().await.records().to_vec();
            bootstrap_saved_peers(&p2p, &records, &options).await
        };
        let peer_statuses = Arc::new(StdRwLock::new(peer_statuses));
        let (p2p_health, _p2p_health_rx) = watch::channel(P2PHealth::default());
        let initial_health = probe_p2p_health(&p2p, &P2PHealth::default()).await;
        p2p_health.send_replace(initial_health);
        let (p2p_control, p2p_control_rx) = mpsc::channel(8);
        let p2p_supervisor = spawn_p2p_supervisor_task(
            Arc::clone(&p2p),
            Arc::clone(&peer_directory),
            Arc::clone(&peer_statuses),
            p2p_health.clone(),
            p2p_control_rx,
            options.install_replicators_on_bootstrap,
        );

        Ok(Self {
            paths,
            options,
            principal,
            node,
            p2p,
            peer_directory,
            store,
            observer: Mutex::new(Some(observer)),
            peer_statuses,
            p2p_supervisor: Mutex::new(Some(p2p_supervisor)),
            p2p_health,
            p2p_control,
            last_mutation_error: StdRwLock::new(None),
            local_peer_id,
            listen_addresses,
            bootstrap_errors,
        })
    }

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

    pub async fn peer_records(&self) -> Vec<super::peer_directory::PeerRecord> {
        self.peer_directory.read().await.records().to_vec()
    }

    pub fn store(&self) -> &Arc<ObservedStore> {
        &self.store
    }

    pub fn store_updates(&self) -> tokio::sync::watch::Receiver<u64> {
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
        self.p2p_control
            .send(P2PSupervisorCommand::RepairNow)
            .await
            .context("queueing desktop P2P repair request")
    }

    pub async fn refresh_store(&self) -> Result<u64> {
        let snapshot = load_full_snapshot(self.node.as_ref()).await?;
        let rows = snapshot.row_count();
        let version = self.store.replace_snapshot(snapshot);
        tracing::debug!(
            target: "defra_agent_desktop::replication",
            version,
            rows,
            "desktop replica snapshot refreshed"
        );
        Ok(version)
    }

    pub async fn shutdown(&self) -> Result<()> {
        if let Some(task) = self.p2p_supervisor.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(observer) = self.observer.lock().await.take() {
            observer.shutdown().await;
        }
        self.node.shutdown().await;

        Ok(())
    }

    pub async fn create_conversation(
        &self,
        agent_did: &str,
        behavior_id: Option<&str>,
    ) -> Result<CreatedConversation> {
        let snapshot = self.store.snapshot();
        match mutations::create_conversation(
            self.node.as_ref(),
            snapshot.as_ref(),
            agent_did,
            behavior_id,
        )
        .await
        {
            Ok(result) => {
                self.store.set_focused_request_id(None);
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    action = "chat_create",
                    row_id = %result.session_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("create conversation", error)),
        }
    }

    pub async fn submit_request(
        &self,
        session_id: &str,
        agent_did: &str,
        content: &str,
        behavior_id: Option<&str>,
    ) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        match mutations::submit_request(
            self.node.as_ref(),
            snapshot.as_ref(),
            session_id,
            agent_did,
            content,
            behavior_id,
        )
        .await
        {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("submit request", error)),
        }
    }

    pub async fn retry_request(&self, parent: &AgentRequestRow) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        match mutations::retry_request(self.node.as_ref(), snapshot.as_ref(), parent).await {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    action = "chat_retry",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("retry request", error)),
        }
    }

    pub async fn add_peer(
        &self,
        label: &str,
        addr: &str,
        agent_did: &str,
    ) -> Result<PeerMutationResult> {
        let label = normalize_required("label", label)?;
        let addr = normalize_required("addr", addr)?;
        let agent_did = normalize_required("agent_did", agent_did)?;

        let record = {
            let mut peer_directory = self.peer_directory.write().await;
            peer_directory
                .upsert_saved_peer(label, addr, agent_did)
                .await?
        };

        let (connected, warning) = match connect_peer_with_retry_until(
            &self.p2p,
            &record.addr,
            &record.label,
            PEER_ADD_OPERATION_TIMEOUT,
        )
        .await
        {
            Ok(()) => match add_replicator_with_retry_until(
                &self.p2p,
                subscribed_collection_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                &record.addr,
                &record.label,
                PEER_ADD_OPERATION_TIMEOUT,
            )
            .await
            {
                Ok(()) => (true, None),
                Err(error) => (
                    true,
                    Some(format!(
                        "peer connected but replication setup failed: {error}"
                    )),
                ),
            },
            Err(error) => (false, Some(format!("peer saved but dial failed: {error}"))),
        };

        self.update_peer_status(ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: connected,
            last_error: warning.clone(),
        });
        self.clear_mutation_error();
        if let Some(warning) = warning.as_deref() {
            tracing::warn!(
                target: "defra_agent_desktop::peer",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %warning,
                "desktop peer add warning"
            );
        } else {
            tracing::info!(
                target: "defra_agent_desktop::peer",
                peer_id = %record.peer_id,
                label = %record.label,
                "desktop peer added"
            );
        }

        Ok(PeerMutationResult {
            peer_id: record.peer_id,
            label: record.label,
            addr: record.addr,
            connected,
            warning,
        })
    }

    pub async fn remove_peer(&self, peer_id: &str) -> Result<PeerMutationResult> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let removed = {
            let mut peer_directory = self.peer_directory.write().await;
            peer_directory.remove(peer_id).await?
        }
        .with_context(|| format!("peer {peer_id} not found"))?;

        let previous_status = {
            let mut statuses = self
                .peer_statuses
                .write()
                .expect("peer status lock poisoned");
            statuses
                .iter()
                .position(|status| status.peer_id == removed.peer_id)
                .map(|index| statuses.remove(index))
        };

        // defra-node's public P2P surface exposes connect_peer but not a
        // disconnect operation, so removing a saved peer only stops future
        // reconnect/bootstrap; any live transport session is left alone.
        let warning = previous_status
            .filter(|status| status.dial_succeeded)
            .map(|_| {
                "saved peer removed; any active transport connection remains until restart"
                    .to_string()
            });

        self.clear_mutation_error();
        tracing::info!(
            target: "defra_agent_desktop::peer",
            peer_id = %removed.peer_id,
            label = %removed.label,
            "desktop peer removed"
        );
        Ok(PeerMutationResult {
            peer_id: removed.peer_id,
            label: removed.label,
            addr: removed.addr,
            connected: false,
            warning,
        })
    }

    pub async fn save_behavior(&self, row: &AgentBehaviorRow) -> Result<()> {
        match mutations::upsert_agent_behavior(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    doc_type = "behavior",
                    row_id = %row.behavior_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save behavior", error)),
        }
    }

    pub async fn save_backend(&self, row: &InferenceBackendRow) -> Result<()> {
        match mutations::upsert_inference_backend(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    doc_type = "backend",
                    row_id = %row.backend_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save backend", error)),
        }
    }

    pub async fn save_tool_selection(&self, row: &ToolSelectionRow) -> Result<()> {
        match mutations::upsert_tool_selection(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    doc_type = "tool_selection",
                    row_id = %row.selection_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tool selection", error)),
        }
    }

    pub async fn save_inference_profile(&self, row: &InferenceProfileRow) -> Result<()> {
        match mutations::upsert_inference_profile(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    doc_type = "inference_profile",
                    row_id = %row.profile_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save inference profile", error)),
        }
    }

    pub async fn save_scheduled_task(&self, row: &ScheduledTaskRow) -> Result<()> {
        match mutations::upsert_scheduled_task(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    doc_type = "scheduled_task",
                    row_id = %row.task_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save scheduled task", error)),
        }
    }

    pub async fn run_scheduled_task_now(&self, row: &ScheduledTaskRow) -> Result<()> {
        match mutations::run_scheduled_task_now(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    doc_type = "scheduled_task",
                    row_id = %row.task_id,
                    action = "run_now",
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("run scheduled task now", error)),
        }
    }

    fn update_peer_status(&self, status: ClientPeerStatus) {
        let mut statuses = self
            .peer_statuses
            .write()
            .expect("peer status lock poisoned");
        if let Some(existing) = statuses
            .iter_mut()
            .find(|existing| existing.peer_id == status.peer_id)
        {
            *existing = status;
        } else {
            statuses.push(status);
            statuses.sort_by(|left, right| {
                left.label
                    .to_lowercase()
                    .cmp(&right.label.to_lowercase())
                    .then_with(|| left.peer_id.cmp(&right.peer_id))
            });
        }
    }

    fn clear_mutation_error(&self) {
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = None;
    }

    fn record_mutation_error(&self, operation: &str, error: anyhow::Error) -> anyhow::Error {
        let message = format!("{operation} failed: {error}");
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = Some(message);
        error
    }
}

async fn bootstrap_saved_peers(
    p2p: &Arc<dyn P2POps>,
    records: &[PeerRecord],
    options: &ClientCoreOptions,
) -> (Vec<ClientPeerStatus>, Vec<String>) {
    let mut statuses = Vec::with_capacity(records.len());
    let mut errors = Vec::new();

    for record in records {
        let mut status = ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: false,
            last_error: None,
        };

        match connect_peer_with_retry(p2p, &record.addr, &record.label).await {
            Ok(()) => {
                status.dial_succeeded = true;

                if options.install_replicators_on_bootstrap {
                    if let Err(error) = add_replicator_with_retry(
                        p2p,
                        subscribed_collection_names()
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                        &record.addr,
                        &record.label,
                    )
                    .await
                    {
                        let message = format!(
                            "peer {} replicator bootstrap failed: {}",
                            record.label, error
                        );
                        status.last_error = Some(message.clone());
                        errors.push(message);
                    }
                }

                if let Some(graphql) = record.graphql.as_deref() {
                    match configure_local_runtime_pairing(p2p, graphql).await {
                        Ok(()) => {}
                        Err(error) => {
                            let message = format!(
                                "peer {} local runtime pairing failed: {}",
                                record.label, error
                            );
                            status.last_error = Some(message.clone());
                            errors.push(message);
                        }
                    }
                }
            }
            Err(error) => {
                let message = format!("peer {} dial failed: {}", record.label, error);
                status.last_error = Some(message.clone());
                errors.push(message);
            }
        }

        statuses.push(status);
    }

    (statuses, errors)
}

async fn configure_local_runtime_pairing(p2p: &Arc<dyn P2POps>, graphql: &str) -> Result<()> {
    let desktop_listen_address = wait_for_bootstrap_listen_address(p2p).await?;
    local_runtime::complete_runtime_pairing(
        graphql,
        &desktop_listen_address,
        subscribed_collection_names()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .await
}

async fn connect_peer_with_retry(p2p: &Arc<dyn P2POps>, addr: &str, label: &str) -> Result<()> {
    connect_peer_with_retry_until(p2p, addr, label, BOOTSTRAP_OPERATION_TIMEOUT).await
}

async fn connect_peer_with_retry_until(
    p2p: &Arc<dyn P2POps>,
    addr: &str,
    label: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let expected_peer_id = parse_public_peer_addr(addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string());

    loop {
        if let Some(peer_id) = expected_peer_id.as_deref() {
            if is_connected_peer(p2p, peer_id).await? {
                return Ok(());
            }
        }

        match p2p_connect_peer(p2p, addr).await {
            Ok(()) => {
                if let Some(peer_id) = expected_peer_id.as_deref() {
                    wait_for_connected_peer(p2p, peer_id, deadline, label).await?;
                }
                return Ok(());
            }
            Err(error) => {
                if let Some(peer_id) = expected_peer_id.as_deref() {
                    if is_connected_peer(p2p, peer_id).await? {
                        return Ok(());
                    }
                }
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out connecting bootstrap peer {label} at {addr}: {error}");
                }
                sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
            }
        }
    }
}

async fn add_replicator_with_retry(
    p2p: &Arc<dyn P2POps>,
    collections: Vec<String>,
    addr: &str,
    label: &str,
) -> Result<()> {
    add_replicator_with_retry_until(p2p, collections, addr, label, BOOTSTRAP_OPERATION_TIMEOUT)
        .await
}

async fn add_replicator_with_retry_until(
    p2p: &Arc<dyn P2POps>,
    collections: Vec<String>,
    addr: &str,
    label: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match p2p_add_replicator(p2p, collections.clone(), addr).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "timed out installing bootstrap replicator for peer {label} at {addr}: {error}"
                    );
                }
                sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
            }
        }
    }
}

async fn wait_for_bootstrap_listen_address(p2p: &Arc<dyn P2POps>) -> Result<String> {
    let deadline = Instant::now() + BOOTSTRAP_OPERATION_TIMEOUT;
    loop {
        let addrs = p2p_listen_addresses(p2p)
            .await
            .context("reading desktop P2P listen addresses for local runtime pairing")?;
        if let Some(addr) = select_local_runtime_pairing_addr(&addrs) {
            return Ok(addr);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("desktop node has no IROH listen address for local runtime pairing");
        }
        sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
    }
}

async fn is_connected_peer(p2p: &Arc<dyn P2POps>, peer_id: &str) -> Result<bool> {
    let peers = p2p_connected_peers(p2p).await?;
    Ok(peers.iter().any(|peer| {
        parse_public_peer_addr(peer)
            .map(|(parsed_peer_id, _)| parsed_peer_id.as_str() == peer_id)
            .unwrap_or_else(|_| peer.contains(peer_id))
    }))
}

async fn wait_for_connected_peer(
    p2p: &Arc<dyn P2POps>,
    peer_id: &str,
    deadline: Instant,
    label: &str,
) -> Result<()> {
    loop {
        if is_connected_peer(p2p, peer_id).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for bootstrap peer {peer_id} to connect for {label}");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

fn spawn_p2p_supervisor_task(
    p2p: Arc<dyn P2POps>,
    peer_directory: Arc<RwLock<PeerDirectory>>,
    peer_statuses: Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    p2p_health: watch::Sender<P2PHealth>,
    mut control_rx: mpsc::Receiver<P2PSupervisorCommand>,
    install_replicators_on_bootstrap: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut health = p2p_health.borrow().clone();
        let mut ticker = tokio::time::interval(P2P_SUPERVISOR_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let manual_repair = tokio::select! {
                _ = ticker.tick() => false,
                command = control_rx.recv() => match command {
                    Some(P2PSupervisorCommand::RepairNow) => true,
                    None => break,
                },
            };

            if manual_repair {
                match p2p_notify_network_change(&p2p).await {
                    Ok(()) => {
                        tracing::info!(
                            target: "defra_agent_desktop::p2p_health",
                            "manual desktop P2P repair requested"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "defra_agent_desktop::p2p_health",
                            error = %error,
                            "manual desktop P2P repair could not refresh network state"
                        );
                    }
                }
            }

            run_saved_peer_repair_cycle(
                &p2p,
                &peer_directory,
                &peer_statuses,
                install_replicators_on_bootstrap,
            )
            .await;

            let next_health = probe_p2p_health(&p2p, &health).await;
            if p2p_health_materially_changed(&health, &next_health) {
                log_p2p_health_transition(&health, &next_health);
                p2p_health.send_replace(next_health.clone());
            }
            health = next_health;
        }
    })
}

async fn run_saved_peer_repair_cycle(
    p2p: &Arc<dyn P2POps>,
    peer_directory: &Arc<RwLock<PeerDirectory>>,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    install_replicators_on_bootstrap: bool,
) {
    let records = peer_directory.read().await.records().to_vec();
    for record in records {
        let current_status = peer_statuses
            .read()
            .expect("peer status lock poisoned")
            .iter()
            .find(|status| status.peer_id == record.peer_id)
            .cloned();

        if !saved_peer_needs_repair(p2p, &record, current_status.as_ref()).await {
            continue;
        }

        let updated = repair_saved_peer(
            p2p,
            &record,
            current_status,
            install_replicators_on_bootstrap,
        )
        .await;
        let still_saved = peer_directory
            .read()
            .await
            .records()
            .iter()
            .any(|candidate| candidate.peer_id == record.peer_id);
        if still_saved {
            replace_peer_status(peer_statuses, updated);
        }
    }
}

async fn probe_p2p_health(p2p: &Arc<dyn P2POps>, previous: &P2PHealth) -> P2PHealth {
    let now = SystemTime::now();
    let probe = async {
        let peer_id = p2p_local_peer_id(p2p).await?;
        if peer_id.trim().is_empty() {
            anyhow::bail!("P2P transport reported an empty peer id");
        }

        let listen_addresses = p2p_listen_addresses(p2p).await?;
        if listen_addresses.is_empty() {
            anyhow::bail!("P2P transport reported no listen addresses");
        }

        let connected_peers = p2p_connected_peers(p2p).await?;
        let replicators = p2p_get_replicators(p2p).await?;

        Ok::<(usize, usize), anyhow::Error>((connected_peers.len(), replicators.len()))
    }
    .await;

    match probe {
        Ok((connected_peer_count, replicator_count)) => P2PHealth {
            status: P2PHealthStatus::Healthy,
            consecutive_failures: 0,
            connected_peer_count,
            replicator_count,
            last_error: None,
            last_ok_at: Some(now),
            last_failure_at: previous.last_failure_at,
        },
        Err(error) => {
            let consecutive_failures = previous.consecutive_failures.saturating_add(1);
            let status = if consecutive_failures >= P2P_WEDGED_FAILURE_THRESHOLD {
                P2PHealthStatus::Wedged
            } else {
                P2PHealthStatus::Degraded
            };
            P2PHealth {
                status,
                consecutive_failures,
                connected_peer_count: previous.connected_peer_count,
                replicator_count: previous.replicator_count,
                last_error: Some(error.to_string()),
                last_ok_at: previous.last_ok_at,
                last_failure_at: Some(now),
            }
        }
    }
}

fn log_p2p_health_transition(previous: &P2PHealth, next: &P2PHealth) {
    if next.status == P2PHealthStatus::Healthy {
        tracing::info!(
            target: "defra_agent_desktop::p2p_health",
            connected_peers = next.connected_peer_count,
            replicators = next.replicator_count,
            "desktop P2P transport is healthy"
        );
        return;
    }

    let error = next
        .last_error
        .as_deref()
        .unwrap_or("unknown transport error");
    let status = next.status.label();
    if next.status != previous.status || previous.last_error.as_deref() != Some(error) {
        tracing::warn!(
            target: "defra_agent_desktop::p2p_health",
            status,
            consecutive_failures = next.consecutive_failures,
            error,
            "desktop P2P transport health degraded"
        );
    }
}

fn p2p_health_materially_changed(previous: &P2PHealth, next: &P2PHealth) -> bool {
    previous.status != next.status
        || previous.consecutive_failures != next.consecutive_failures
        || previous.connected_peer_count != next.connected_peer_count
        || previous.replicator_count != next.replicator_count
        || previous.last_error != next.last_error
}

async fn saved_peer_needs_repair(
    p2p: &Arc<dyn P2POps>,
    record: &PeerRecord,
    status: Option<&ClientPeerStatus>,
) -> bool {
    if status.is_none()
        || status.is_some_and(|status| !status.dial_succeeded || status.last_error.is_some())
    {
        return true;
    }

    let Some(expected_peer_id) = parse_public_peer_addr(&record.addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
    else {
        return false;
    };

    match is_connected_peer(p2p, &expected_peer_id).await {
        Ok(connected) => !connected,
        Err(error) => {
            tracing::debug!(
                target: "defra_agent_desktop::peer_maintenance",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %error,
                "failed to check live P2P connectivity; forcing repair"
            );
            true
        }
    }
}

async fn repair_saved_peer(
    p2p: &Arc<dyn P2POps>,
    record: &PeerRecord,
    current_status: Option<ClientPeerStatus>,
    install_replicators_on_bootstrap: bool,
) -> ClientPeerStatus {
    let mut status = current_status.unwrap_or_else(|| ClientPeerStatus {
        peer_id: record.peer_id.clone(),
        label: record.label.clone(),
        agent_did: record.agent_did.clone(),
        addr: record.addr.clone(),
        dial_succeeded: false,
        last_error: None,
    });

    let expected_peer_id = parse_public_peer_addr(&record.addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string());
    let connected_now = match expected_peer_id.as_deref() {
        Some(peer_id) => is_connected_peer(p2p, peer_id).await.unwrap_or(false),
        None => status.dial_succeeded,
    };

    if !connected_now {
        match p2p_notify_network_change(p2p).await {
            Ok(()) => {
                tracing::debug!(
                    target: "defra_agent_desktop::peer_maintenance",
                    peer_id = %record.peer_id,
                    label = %record.label,
                    "refreshed P2P network state before reconnect"
                );
            }
            Err(error) => {
                tracing::debug!(
                    target: "defra_agent_desktop::peer_maintenance",
                    peer_id = %record.peer_id,
                    label = %record.label,
                    error = %error,
                    "failed to refresh P2P network state before reconnect"
                );
            }
        }

        match connect_peer_with_retry(p2p, &record.addr, &record.label).await {
            Ok(()) => {
                status.dial_succeeded = true;
                if install_replicators_on_bootstrap {
                    if let Err(error) = add_replicator_with_retry(
                        p2p,
                        subscribed_collection_names()
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                        &record.addr,
                        &record.label,
                    )
                    .await
                    {
                        status.last_error = Some(format!(
                            "peer {} replicator bootstrap failed: {}",
                            record.label, error
                        ));
                        return status;
                    }
                }
            }
            Err(error) => {
                status.dial_succeeded = false;
                status.last_error = Some(format!("peer {} dial failed: {}", record.label, error));
                return status;
            }
        }
    } else {
        status.dial_succeeded = true;
    }

    if let Some(graphql) = record.graphql.as_deref() {
        match configure_local_runtime_pairing(p2p, graphql).await {
            Ok(()) => status.last_error = None,
            Err(error) => {
                status.last_error = Some(format!(
                    "peer {} local runtime pairing failed: {}",
                    record.label, error
                ));
            }
        }
    } else {
        status.last_error = None;
    }

    status
}

fn replace_peer_status(
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    status: ClientPeerStatus,
) {
    let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
    if let Some(existing) = statuses
        .iter_mut()
        .find(|existing| existing.peer_id == status.peer_id)
    {
        *existing = status;
    } else {
        statuses.push(status);
        statuses.sort_by(|left, right| {
            left.label
                .to_lowercase()
                .cmp(&right.label.to_lowercase())
                .then_with(|| left.peer_id.cmp(&right.peer_id))
        });
    }
}

async fn p2p_local_peer_id(p2p: &Arc<dyn P2POps>) -> Result<String> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.local_peer_id()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P peer id"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P peer id"),
    }
}

async fn p2p_listen_addresses(p2p: &Arc<dyn P2POps>) -> Result<Vec<String>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.listen_addresses()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P listen addresses"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P listen addresses"),
    }
}

async fn p2p_connected_peers(p2p: &Arc<dyn P2POps>) -> Result<Vec<String>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.connected_peers()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P connected peers"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P connected peers"),
    }
}

async fn p2p_connect_peer(p2p: &Arc<dyn P2POps>, addr: &str) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.connect_peer(addr)).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("connecting desktop P2P peer {addr}")),
        Err(_) => anyhow::bail!("timed out connecting desktop P2P peer {addr}"),
    }
}

async fn p2p_notify_network_change(p2p: &Arc<dyn P2POps>) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.notify_network_change()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("refreshing desktop P2P network state"),
        Err(_) => anyhow::bail!("timed out refreshing desktop P2P network state"),
    }
}

async fn p2p_get_replicators(
    p2p: &Arc<dyn P2POps>,
) -> Result<Vec<defra_p2p_adapter::ReplicatorInfo>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.get_replicators()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P replicators"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P replicators"),
    }
}

async fn p2p_add_replicator(
    p2p: &Arc<dyn P2POps>,
    collections: Vec<String>,
    addr: &str,
) -> Result<()> {
    match timeout(
        P2P_OPERATION_TIMEOUT,
        p2p.add_replicator(collections, Some(addr), Vec::new(), None),
    )
    .await
    {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("adding desktop P2P replicator for {addr}")),
        Err(_) => anyhow::bail!("timed out adding desktop P2P replicator for {addr}"),
    }
}

fn select_local_runtime_pairing_addr(addrs: &[String]) -> Option<String> {
    let candidates = addrs
        .iter()
        .map(|addr| addr.trim())
        .filter(|addr| !addr.is_empty())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    candidates
        .iter()
        .find(|addr| addr_has_loopback_hint(addr))
        .map(|addr| (*addr).to_string())
        .or_else(|| candidates.first().map(|addr| (*addr).to_string()))
}

fn addr_has_loopback_hint(addr: &str) -> bool {
    parse_public_peer_addr(addr)
        .ok()
        .map(|(_, hints)| {
            hints.iter().any(|hint| {
                hint.as_str()
                    .parse::<SocketAddr>()
                    .map(|socket| socket.ip().is_loopback())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use defra_p2p_adapter::{
        ExplicitReplayCapabilityInput, P2PResult, P2pDocumentInfo, P2pDocumentRequest,
        ReplicatorInfo,
    };

    use super::*;

    #[derive(Default)]
    struct RecordingP2P {
        notify_calls: AtomicUsize,
        local_peer_id_error: StdRwLock<Option<String>>,
        listen_addresses: StdRwLock<Vec<String>>,
        listen_addresses_error: StdRwLock<Option<String>>,
        connected_peers: StdRwLock<Vec<String>>,
        connected_peers_error: StdRwLock<Option<String>>,
        connect_calls: StdRwLock<Vec<String>>,
        replicators: StdRwLock<Vec<ReplicatorInfo>>,
        replicators_error: StdRwLock<Option<String>>,
    }

    #[allow(dead_code)]
    impl RecordingP2P {
        fn notify_calls(&self) -> usize {
            self.notify_calls.load(Ordering::SeqCst)
        }

        fn set_local_peer_id_error(&self, error: Option<&str>) {
            *self
                .local_peer_id_error
                .write()
                .expect("local peer id error lock poisoned") = error.map(ToOwned::to_owned);
        }

        fn set_listen_addresses(&self, addrs: Vec<String>) {
            *self
                .listen_addresses
                .write()
                .expect("listen addresses lock poisoned") = addrs;
        }

        fn set_listen_addresses_error(&self, error: Option<&str>) {
            *self
                .listen_addresses_error
                .write()
                .expect("listen addresses error lock poisoned") = error.map(ToOwned::to_owned);
        }

        fn set_connected_peers(&self, peers: Vec<String>) {
            *self
                .connected_peers
                .write()
                .expect("connected peers lock poisoned") = peers;
        }

        fn set_connected_peers_error(&self, error: Option<&str>) {
            *self
                .connected_peers_error
                .write()
                .expect("connected peers error lock poisoned") = error.map(ToOwned::to_owned);
        }

        fn connected_peer_snapshot(&self) -> Vec<String> {
            self.connected_peers
                .read()
                .expect("connected peers lock poisoned")
                .clone()
        }

        fn connect_calls(&self) -> Vec<String> {
            self.connect_calls
                .read()
                .expect("connect calls lock poisoned")
                .clone()
        }

        fn set_replicators(&self, replicators: Vec<ReplicatorInfo>) {
            *self.replicators.write().expect("replicators lock poisoned") = replicators;
        }

        fn set_replicators_error(&self, error: Option<&str>) {
            *self
                .replicators_error
                .write()
                .expect("replicators error lock poisoned") = error.map(ToOwned::to_owned);
        }
    }

    #[async_trait]
    impl P2POps for RecordingP2P {
        async fn local_peer_id(&self) -> P2PResult<String> {
            if let Some(error) = self
                .local_peer_id_error
                .read()
                .expect("local peer id error lock poisoned")
                .clone()
            {
                return Err(error.into());
            }
            Ok("local-peer".to_string())
        }

        async fn listen_addresses(&self) -> P2PResult<Vec<String>> {
            if let Some(error) = self
                .listen_addresses_error
                .read()
                .expect("listen addresses error lock poisoned")
                .clone()
            {
                return Err(error.into());
            }
            Ok(self
                .listen_addresses
                .read()
                .expect("listen addresses lock poisoned")
                .clone())
        }

        async fn connected_peers(&self) -> P2PResult<Vec<String>> {
            if let Some(error) = self
                .connected_peers_error
                .read()
                .expect("connected peers error lock poisoned")
                .clone()
            {
                return Err(error.into());
            }
            Ok(self.connected_peer_snapshot())
        }

        async fn connect_peer(&self, addr: &str) -> P2PResult<()> {
            self.connect_calls
                .write()
                .expect("connect calls lock poisoned")
                .push(addr.to_string());
            self.connected_peers
                .write()
                .expect("connected peers lock poisoned")
                .push(addr.to_string());
            Ok(())
        }

        async fn notify_network_change(&self) -> P2PResult<()> {
            self.notify_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn get_replicators(&self) -> P2PResult<Vec<ReplicatorInfo>> {
            if let Some(error) = self
                .replicators_error
                .read()
                .expect("replicators error lock poisoned")
                .clone()
            {
                return Err(error.into());
            }
            Ok(self
                .replicators
                .read()
                .expect("replicators lock poisoned")
                .clone())
        }

        async fn add_replicator(
            &self,
            _collections: Vec<String>,
            _addr: Option<&str>,
            _explicit_replay_capabilities: Vec<ExplicitReplayCapabilityInput>,
            _expected_authorizer_did: Option<&str>,
        ) -> P2PResult<()> {
            Ok(())
        }

        async fn remove_replicator(
            &self,
            _collections: Vec<String>,
            _addr: Option<&str>,
        ) -> P2PResult<()> {
            Ok(())
        }

        async fn get_collections(&self) -> P2PResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn add_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
            Ok(())
        }

        async fn remove_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
            Ok(())
        }

        async fn get_documents(&self) -> P2PResult<Vec<P2pDocumentInfo>> {
            Ok(Vec::new())
        }

        async fn add_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
            Ok(())
        }

        async fn remove_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
            Ok(())
        }

        async fn republish_document(&self, _collection_name: &str, _doc_id: &str) -> P2PResult<()> {
            Ok(())
        }

        async fn sync_documents(
            &self,
            _collection_name: &str,
            _doc_ids: Vec<String>,
        ) -> P2PResult<()> {
            Ok(())
        }

        async fn sync_branchable_collection(&self, _collection_id: &str) -> P2PResult<()> {
            Ok(())
        }

        async fn sync_collection_versions(&self, _version_ids: Vec<String>) -> P2PResult<()> {
            Ok(())
        }
    }

    #[test]
    fn select_local_runtime_pairing_addr_prefers_loopback() {
        let selected = select_local_runtime_pairing_addr(&[
            "100.111.156.102:56000/p2p/peer-alpha".to_string(),
            "127.0.0.1:56000/p2p/peer-alpha".to_string(),
            "peer-alpha".to_string(),
        ]);

        assert_eq!(selected.as_deref(), Some("127.0.0.1:56000/p2p/peer-alpha"));
    }

    #[test]
    fn select_local_runtime_pairing_addr_falls_back_to_first_nonempty() {
        let selected = select_local_runtime_pairing_addr(&[
            "endpointabc123".to_string(),
            "peer-alpha".to_string(),
        ]);

        assert_eq!(selected.as_deref(), Some("endpointabc123"));
    }

    #[tokio::test]
    async fn repair_saved_peer_refreshes_network_before_redial() {
        let recording = Arc::new(RecordingP2P::default());
        let p2p: Arc<dyn P2POps> = recording.clone();
        let record = PeerRecord::new(
            "Workshop Bay",
            "127.0.0.1:56000/p2p/peer-alpha",
            "did:defra:workshop-bay",
        );

        let repaired = repair_saved_peer(
            &p2p,
            &record,
            Some(ClientPeerStatus {
                peer_id: record.peer_id.clone(),
                label: record.label.clone(),
                agent_did: record.agent_did.clone(),
                addr: record.addr.clone(),
                dial_succeeded: false,
                last_error: Some("peer Workshop Bay dial failed".to_string()),
            }),
            false,
        )
        .await;

        assert_eq!(recording.notify_calls(), 1);
        assert_eq!(recording.connect_calls(), vec![record.addr.clone()]);
        assert!(repaired.dial_succeeded);
        assert_eq!(repaired.last_error, None);
    }

    #[tokio::test]
    async fn saved_peer_needs_repair_when_live_connection_has_dropped() {
        let recording = Arc::new(RecordingP2P::default());
        let p2p: Arc<dyn P2POps> = recording.clone();
        let record = PeerRecord::new(
            "Workshop Bay",
            "127.0.0.1:56000/p2p/peer-alpha",
            "did:defra:workshop-bay",
        );

        let needs_repair = saved_peer_needs_repair(
            &p2p,
            &record,
            Some(&ClientPeerStatus {
                peer_id: record.peer_id.clone(),
                label: record.label.clone(),
                agent_did: record.agent_did.clone(),
                addr: record.addr.clone(),
                dial_succeeded: true,
                last_error: None,
            }),
        )
        .await;

        assert!(needs_repair);
    }

    #[tokio::test]
    async fn saved_peer_does_not_need_repair_while_live_connection_is_healthy() {
        let recording = Arc::new(RecordingP2P::default());
        recording.set_connected_peers(vec!["127.0.0.1:56000/p2p/peer-alpha".to_string()]);
        let p2p: Arc<dyn P2POps> = recording.clone();
        let record = PeerRecord::new(
            "Workshop Bay",
            "127.0.0.1:56000/p2p/peer-alpha",
            "did:defra:workshop-bay",
        );

        let needs_repair = saved_peer_needs_repair(
            &p2p,
            &record,
            Some(&ClientPeerStatus {
                peer_id: record.peer_id.clone(),
                label: record.label.clone(),
                agent_did: record.agent_did.clone(),
                addr: record.addr.clone(),
                dial_succeeded: true,
                last_error: None,
            }),
        )
        .await;

        assert!(!needs_repair);
    }

    #[tokio::test]
    async fn probe_p2p_health_reports_healthy_transport() {
        let recording = Arc::new(RecordingP2P::default());
        recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
        recording.set_connected_peers(vec!["127.0.0.1:56000/p2p/peer-alpha".to_string()]);
        recording.set_replicators(vec![ReplicatorInfo {
            id: Some("peer-alpha".to_string()),
            collections: vec!["AgentRequest".to_string()],
            address: Some("127.0.0.1:56000/p2p/peer-alpha".to_string()),
        }]);
        let p2p: Arc<dyn P2POps> = recording;

        let health = probe_p2p_health(&p2p, &P2PHealth::default()).await;

        assert_eq!(health.status, P2PHealthStatus::Healthy);
        assert_eq!(health.connected_peer_count, 1);
        assert_eq!(health.replicator_count, 1);
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(health.last_error, None);
        assert!(health.last_ok_at.is_some());
    }

    #[tokio::test]
    async fn probe_p2p_health_marks_repeated_failures_wedged() {
        let recording = Arc::new(RecordingP2P::default());
        recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
        recording.set_connected_peers_error(Some("channel send error"));
        let p2p: Arc<dyn P2POps> = recording;

        let mut health = P2PHealth::default();
        for _ in 0..P2P_WEDGED_FAILURE_THRESHOLD {
            health = probe_p2p_health(&p2p, &health).await;
        }

        assert_eq!(health.status, P2PHealthStatus::Wedged);
        assert_eq!(health.consecutive_failures, P2P_WEDGED_FAILURE_THRESHOLD);
        assert_eq!(
            health.last_error.as_deref(),
            Some("reading desktop P2P connected peers")
        );
        assert!(health.last_failure_at.is_some());
    }

    #[test]
    fn p2p_health_materially_changed_ignores_probe_timestamps() {
        let previous = P2PHealth {
            status: P2PHealthStatus::Healthy,
            consecutive_failures: 0,
            connected_peer_count: 1,
            replicator_count: 1,
            last_error: None,
            last_ok_at: Some(SystemTime::UNIX_EPOCH),
            last_failure_at: None,
        };
        let next = P2PHealth {
            last_ok_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2)),
            ..previous.clone()
        };

        assert!(!p2p_health_materially_changed(&previous, &next));
    }

    #[test]
    fn p2p_health_materially_changed_detects_live_topology_change() {
        let previous = P2PHealth {
            status: P2PHealthStatus::Healthy,
            consecutive_failures: 0,
            connected_peer_count: 1,
            replicator_count: 1,
            last_error: None,
            last_ok_at: Some(SystemTime::UNIX_EPOCH),
            last_failure_at: None,
        };
        let next = P2PHealth {
            connected_peer_count: 2,
            ..previous.clone()
        };

        assert!(p2p_health_materially_changed(&previous, &next));
    }
}
