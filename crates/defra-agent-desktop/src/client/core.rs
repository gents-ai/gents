use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentRequestRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow,
    ToolSelectionRow,
};
use defra_node::{EmbeddedNode, NodeBuilder, P2PConfig};
use defra_p2p_adapter::P2POperations as P2POps;
use p2p::iroh::{parse_public_peer_addr, IrohDiscoveryConfig, IrohRelayModeConfig};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Instant};

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
const BOOTSTRAP_OPERATION_BACKOFF: Duration = Duration::from_millis(250);
const PEER_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);

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
            max_concurrent_push_tasks: 8,
            rate_limit_burst: 500,
            rate_limit_rate: 50.0,
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

pub struct ClientCore {
    paths: DesktopPaths,
    principal: PrincipalIdentity,
    node: Arc<EmbeddedNode>,
    p2p: Arc<dyn P2POps>,
    peer_directory: Arc<RwLock<PeerDirectory>>,
    store: Arc<ObservedStore>,
    observer: Mutex<Option<ObserverHandle>>,
    peer_statuses: Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    peer_maintenance: Mutex<Option<JoinHandle<()>>>,
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
        let local_peer_id = p2p
            .local_peer_id()
            .await
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P peer id")?;
        let listen_addresses = p2p
            .listen_addresses()
            .await
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P listen addresses")?;

        let (peer_statuses, _peer_errors) = {
            let records = peer_directory.read().await.records().to_vec();
            bootstrap_saved_peers(&p2p, &records, &options).await
        };
        let peer_statuses = Arc::new(StdRwLock::new(peer_statuses));
        let peer_maintenance = spawn_peer_maintenance_task(
            Arc::clone(&p2p),
            Arc::clone(&peer_directory),
            Arc::clone(&peer_statuses),
            options.install_replicators_on_bootstrap,
        );

        Ok(Self {
            paths,
            principal,
            node,
            p2p,
            peer_directory,
            store,
            observer: Mutex::new(Some(observer)),
            peer_statuses,
            peer_maintenance: Mutex::new(Some(peer_maintenance)),
            last_mutation_error: StdRwLock::new(None),
            local_peer_id,
            listen_addresses,
            bootstrap_errors,
        })
    }

    pub fn paths(&self) -> &DesktopPaths {
        &self.paths
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
        if let Some(task) = self.peer_maintenance.lock().await.take() {
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

        let (connected, warning) = match self.p2p.connect_peer(&record.addr).await {
            Ok(()) => match self
                .p2p
                .add_replicator(
                    subscribed_collection_names()
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    Some(&record.addr),
                    Vec::new(),
                    None,
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
    let deadline = Instant::now() + BOOTSTRAP_OPERATION_TIMEOUT;
    let expected_peer_id = parse_public_peer_addr(addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string());

    loop {
        if let Some(peer_id) = expected_peer_id.as_deref() {
            if is_connected_peer(p2p, peer_id).await? {
                return Ok(());
            }
        }

        match p2p.connect_peer(addr).await {
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
    let deadline = Instant::now() + BOOTSTRAP_OPERATION_TIMEOUT;
    loop {
        match p2p
            .add_replicator(collections.clone(), Some(addr), Vec::new(), None)
            .await
        {
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
        let addrs = p2p
            .listen_addresses()
            .await
            .map_err(anyhow::Error::msg)
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
    let peers = p2p.connected_peers().await.map_err(anyhow::Error::msg)?;
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

fn spawn_peer_maintenance_task(
    p2p: Arc<dyn P2POps>,
    peer_directory: Arc<RwLock<PeerDirectory>>,
    peer_statuses: Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    install_replicators_on_bootstrap: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            sleep(PEER_MAINTENANCE_INTERVAL).await;

            let records = peer_directory.read().await.records().to_vec();
            for record in records {
                let current_status = peer_statuses
                    .read()
                    .expect("peer status lock poisoned")
                    .iter()
                    .find(|status| status.peer_id == record.peer_id)
                    .cloned();

                if !needs_pairing_repair(&record, current_status.as_ref()) {
                    continue;
                }

                let updated = repair_saved_peer(
                    &p2p,
                    &record,
                    current_status,
                    install_replicators_on_bootstrap,
                )
                .await;
                replace_peer_status(&peer_statuses, updated);
            }
        }
    })
}

fn needs_pairing_repair(record: &PeerRecord, status: Option<&ClientPeerStatus>) -> bool {
    status.is_none()
        || status.is_some_and(|status| !status.dial_succeeded || status.last_error.is_some())
        || (record.graphql.is_some() && status.is_some_and(|status| status.last_error.is_some()))
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
        match p2p.notify_network_change().await {
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
        connect_calls: StdRwLock<Vec<String>>,
    }

    impl RecordingP2P {
        fn notify_calls(&self) -> usize {
            self.notify_calls.load(Ordering::SeqCst)
        }

        fn connect_calls(&self) -> Vec<String> {
            self.connect_calls
                .read()
                .expect("connect calls lock poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl P2POps for RecordingP2P {
        async fn local_peer_id(&self) -> P2PResult<String> {
            Ok("local-peer".to_string())
        }

        async fn listen_addresses(&self) -> P2PResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn connected_peers(&self) -> P2PResult<Vec<String>> {
            Ok(self.connect_calls())
        }

        async fn connect_peer(&self, addr: &str) -> P2PResult<()> {
            self.connect_calls
                .write()
                .expect("connect calls lock poisoned")
                .push(addr.to_string());
            Ok(())
        }

        async fn notify_network_change(&self) -> P2PResult<()> {
            self.notify_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn get_replicators(&self) -> P2PResult<Vec<ReplicatorInfo>> {
            Ok(Vec::new())
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
}
