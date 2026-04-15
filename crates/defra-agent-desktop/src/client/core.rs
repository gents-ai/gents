use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use anyhow::{Context, Result};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentRequestRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow,
    ToolSelectionRow,
};
use defra_node::{EmbeddedNode, NodeBuilder, P2PConfig, P2POps};
use p2p::iroh::{IrohDiscoveryConfig, IrohRelayModeConfig};
use tokio::sync::{Mutex, RwLock};

use super::mutations::{self, CreatedConversation, PeerMutationResult, SubmittedRequest};
use super::observe::{spawn_observer, ObservedStore, ObserverHandle};
use super::paths::DesktopPaths;
use super::peer_directory::PeerDirectory;
use super::principal_identity::PrincipalIdentity;
use super::query::load_full_snapshot;
use super::schema::{
    ensure_runtime_schemas, subscribe_all_collections, subscribed_collection_names,
};

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
    peer_directory: RwLock<PeerDirectory>,
    store: Arc<ObservedStore>,
    observer: Mutex<Option<ObserverHandle>>,
    peer_statuses: StdRwLock<Vec<ClientPeerStatus>>,
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
        let mut bootstrap_errors = Vec::new();
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

        let peer_directory = PeerDirectory::load(paths.peer_directory_path()).await?;
        let p2p = node
            .p2p_arc()
            .context("desktop node started without P2P support")?;
        let local_peer_id = p2p.local_peer_id().await;
        let listen_addresses = p2p.listen_addresses().await;

        let (peer_statuses, peer_errors) =
            bootstrap_saved_peers(&p2p, peer_directory.records(), &options).await;
        bootstrap_errors.extend(peer_errors);

        Ok(Self {
            paths,
            principal,
            node,
            p2p,
            peer_directory: RwLock::new(peer_directory),
            store,
            observer: Mutex::new(Some(observer)),
            peer_statuses: StdRwLock::new(peer_statuses),
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

    #[cfg(test)]
    pub(crate) fn add_test_peer_status(
        &self,
        label: impl Into<String>,
        addr: impl Into<String>,
        agent_did: impl Into<String>,
        dial_succeeded: bool,
    ) -> ClientPeerStatus {
        let status = ClientPeerStatus {
            peer_id: uuid::Uuid::new_v4().to_string(),
            label: label.into(),
            agent_did: agent_did.into(),
            addr: addr.into(),
            dial_succeeded,
            last_error: None,
        };
        self.peer_statuses
            .write()
            .expect("peer status lock poisoned")
            .push(status.clone());
        status
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
        tracing::info!(
            target: "defra_agent_desktop::replication",
            version,
            rows,
            "desktop replica snapshot refreshed"
        );
        Ok(version)
    }

    pub async fn shutdown(&self) -> Result<()> {
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
                .set_replicator(
                    &record.addr,
                    subscribed_collection_names()
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
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
    records: &[super::peer_directory::PeerRecord],
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

        match p2p.connect_peer(&record.addr).await {
            Ok(()) => {
                status.dial_succeeded = true;

                if options.install_replicators_on_bootstrap {
                    if let Err(error) = p2p
                        .set_replicator(
                            &record.addr,
                            subscribed_collection_names()
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
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

fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}
