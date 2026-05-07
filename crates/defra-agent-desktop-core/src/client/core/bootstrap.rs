use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, NodeBuilder, P2PConfig, StorageBackend};
use defra_p2p_adapter::P2POperations as P2POps;
use p2p::iroh::parse_public_peer_addr;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Instant};

use super::super::observe::{spawn_observer_with_selection, ObservedStore};
use super::super::paths::DesktopPaths;
use super::super::peer_directory::{PeerDirectory, PeerRecord};
use super::super::principal_identity::PrincipalIdentity;
use super::super::query::load_full_snapshot_with_peer_records;
use super::super::schema::{
    branchable_collection_names, ensure_runtime_schemas, subscribe_all_collections,
    subscribed_collection_names,
};
use super::materialization::spawn_materialization_supervisor_task;
use super::p2p_ops::{
    p2p_add_replicator, p2p_connect_peer, p2p_connected_peers, p2p_listen_addresses,
    p2p_local_peer_id, p2p_sync_branchable_collection,
};
use super::supervisor::spawn_p2p_supervisor_task;
use super::{
    ClientCore, ClientCoreOptions, ClientPeerStatus, P2PHealth, BOOTSTRAP_OPERATION_BACKOFF,
    BOOTSTRAP_OPERATION_TIMEOUT,
};
use crate::local_runtime;

pub(super) const BRANCHABLE_PAIR_SYNC_ENV: &str = "DEFRA_AGENT_DESKTOP_SYNC_BRANCHABLE_ON_PAIR";
pub(super) const REMOTE_P2P_PAIRING_ENV: &str = "DEFRA_AGENT_DESKTOP_PAIR_REMOTE_P2P";

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

        let peer_directory = Arc::new(tokio::sync::RwLock::new(
            PeerDirectory::load(paths.peer_directory_path()).await?,
        ));
        ensure_runtime_schemas(node.as_ref()).await?;
        subscribe_all_collections(node.as_ref()).await?;

        // Open the EventName::Update subscription BEFORE reading the
        // bootstrap snapshot. Writes that land between subscribe and the
        // snapshot read are buffered in the bounded mpsc and drained by
        // the observer on first tick. merge_snapshot is idempotent so
        // duplicates are harmless.
        let observer_subscription = node.subscribe(&[defra_node::EventName::Update]);

        // Create the selection channel BEFORE the observer is spawned so the
        // observer can use the receiver for scoped drop-recovery reloads.
        let (selected_agent_did, _) = watch::channel::<Option<String>>(None);

        let initial_snapshot = {
            let records = peer_directory.read().await.records().to_vec();
            load_full_snapshot_with_peer_records(node.as_ref(), &records).await?
        };
        let (store, _store_updates) = ObservedStore::new(initial_snapshot);
        let observer = spawn_observer_with_selection(
            Arc::clone(&node),
            Arc::clone(&store),
            Arc::clone(&peer_directory),
            observer_subscription,
            selected_agent_did.subscribe(),
        );

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
            bootstrap_saved_peers(node.as_ref(), &p2p, &records, &options).await
        };
        let peer_statuses = Arc::new(std::sync::RwLock::new(peer_statuses));
        let (p2p_health, _p2p_health_rx) = watch::channel(P2PHealth::default());
        let initial_health = super::supervisor::probe_p2p_health(&p2p, &P2PHealth::default()).await;
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
        let materialization_supervisor = spawn_materialization_supervisor_task(
            Arc::clone(&node),
            Arc::clone(&p2p),
            Arc::clone(&store),
        );

        Ok(Self {
            paths,
            options,
            principal,
            node,
            p2p,
            peer_directory,
            store,
            observer: tokio::sync::Mutex::new(Some(observer)),
            peer_statuses,
            p2p_supervisor: tokio::sync::Mutex::new(Some(p2p_supervisor)),
            materialization_supervisor: tokio::sync::Mutex::new(Some(materialization_supervisor)),
            p2p_health,
            selected_agent_did,
            p2p_control: tokio::sync::Mutex::new(Some(p2p_control)),
            last_mutation_error: std::sync::RwLock::new(None),
            local_peer_id,
            listen_addresses,
            bootstrap_errors,
        })
    }
}

pub(super) async fn bootstrap_saved_peers(
    node: &EmbeddedNode,
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

                let p2p_pairing_enabled = record
                    .graphql
                    .as_deref()
                    .map(p2p_pairing_enabled_for_graphql)
                    .unwrap_or(true);

                if options.install_replicators_on_bootstrap && p2p_pairing_enabled {
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
                } else if record.graphql.is_some() && !p2p_pairing_enabled {
                    tracing::info!(
                        target: "defra_agent_desktop_core::peer",
                        label = %record.label,
                        env = REMOTE_P2P_PAIRING_ENV,
                        "skipping automatic remote P2P replicator bootstrap for GraphQL-managed peer"
                    );
                }

                if let Some(graphql) = record.graphql.as_deref() {
                    if p2p_pairing_enabled {
                        match configure_local_runtime_pairing(p2p, graphql).await {
                            Ok(()) => {
                                if branchable_pair_sync_enabled() {
                                    match sync_branchable_collections_with_retry(
                                        node,
                                        p2p,
                                        &record.label,
                                        BOOTSTRAP_OPERATION_TIMEOUT,
                                    )
                                    .await
                                    {
                                        Ok(synced) => {
                                            tracing::info!(
                                                target: "defra_agent_desktop_core::peer",
                                                label = %record.label,
                                                synced_collections = ?synced,
                                                "desktop requested branchable collection sync after pairing"
                                            );
                                        }
                                        Err(error) => {
                                            let message = format!(
                                                "peer {} branchable sync failed: {}",
                                                record.label, error
                                            );
                                            status.last_error = Some(message.clone());
                                            errors.push(message);
                                        }
                                    }
                                } else {
                                    tracing::debug!(
                                        target: "defra_agent_desktop_core::peer",
                                        label = %record.label,
                                        env = BRANCHABLE_PAIR_SYNC_ENV,
                                        "skipping opt-in branchable collection sync after pairing"
                                    );
                                }
                            }
                            Err(error) => {
                                let message = format!(
                                    "peer {} local runtime pairing failed: {}",
                                    record.label, error
                                );
                                status.last_error = Some(message.clone());
                                errors.push(message);
                            }
                        }
                    } else {
                        tracing::info!(
                            target: "defra_agent_desktop_core::peer",
                            label = %record.label,
                            graphql,
                            env = REMOTE_P2P_PAIRING_ENV,
                            "skipping automatic reverse P2P pairing for GraphQL-managed peer"
                        );
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

pub(super) async fn configure_local_runtime_pairing(
    p2p: &Arc<dyn P2POps>,
    graphql: &str,
) -> Result<()> {
    let desktop_listen_address = wait_for_bootstrap_listen_address(p2p, graphql).await?;
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

pub(super) async fn connect_peer_with_retry(
    p2p: &Arc<dyn P2POps>,
    addr: &str,
    label: &str,
) -> Result<()> {
    connect_peer_with_retry_until(p2p, addr, label, BOOTSTRAP_OPERATION_TIMEOUT).await
}

pub(super) async fn force_connect_peer_with_retry(
    p2p: &Arc<dyn P2POps>,
    addr: &str,
    label: &str,
) -> Result<()> {
    force_connect_peer_with_retry_until(p2p, addr, label, BOOTSTRAP_OPERATION_TIMEOUT).await
}

pub(super) async fn connect_peer_with_retry_until(
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

pub(super) async fn force_connect_peer_with_retry_until(
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
                    anyhow::bail!(
                        "timed out force-connecting bootstrap peer {label} at {addr}: {error}"
                    );
                }
                sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
            }
        }
    }
}

pub(super) async fn add_replicator_with_retry(
    p2p: &Arc<dyn P2POps>,
    collections: Vec<String>,
    addr: &str,
    label: &str,
) -> Result<()> {
    add_replicator_with_retry_until(p2p, collections, addr, label, BOOTSTRAP_OPERATION_TIMEOUT)
        .await
}

pub(super) async fn add_replicator_with_retry_until(
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

pub(super) async fn sync_branchable_collections_with_retry(
    node: &EmbeddedNode,
    p2p: &Arc<dyn P2POps>,
    label: &str,
    timeout: Duration,
) -> Result<Vec<String>> {
    let deadline = Instant::now() + timeout;
    let mut synced = Vec::new();
    for collection_name in branchable_collection_names() {
        let collection_id = node
            .get_collection(collection_name)
            .map_err(|error| {
                anyhow::anyhow!("loading collection id for {collection_name}: {error}")
            })?
            .ok_or_else(|| anyhow::anyhow!("collection {collection_name} not found"))?
            .collection_id;

        loop {
            match p2p_sync_branchable_collection(p2p, &collection_id).await {
                Ok(()) => {
                    synced.push(collection_name.to_string());
                    break;
                }
                Err(error) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "timed out syncing branchable collection {collection_name} for peer {label}: {error}"
                        );
                    }
                    sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
                }
            }
        }
    }
    Ok(synced)
}

pub(super) fn branchable_pair_sync_enabled() -> bool {
    env_flag_enabled(BRANCHABLE_PAIR_SYNC_ENV)
}

pub(super) fn p2p_pairing_enabled_for_graphql(graphql: &str) -> bool {
    graphql_endpoint_is_loopback_or_unspecified(graphql) || env_flag_enabled(REMOTE_P2P_PAIRING_ENV)
}

fn env_flag_enabled(name: &str) -> bool {
    let Ok(value) = std::env::var(name) else {
        return false;
    };
    let value = value.trim().to_ascii_lowercase();
    matches!(value.as_str(), "1" | "true" | "yes" | "on")
}

async fn wait_for_bootstrap_listen_address(p2p: &Arc<dyn P2POps>, graphql: &str) -> Result<String> {
    let deadline = Instant::now() + BOOTSTRAP_OPERATION_TIMEOUT;
    loop {
        let addrs = p2p_listen_addresses(p2p)
            .await
            .context("reading desktop P2P listen addresses for local runtime pairing")?;
        if let Some(addr) = select_runtime_pairing_addr(&addrs, graphql) {
            return Ok(addr);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("desktop node has no IROH listen address for local runtime pairing");
        }
        sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
    }
}

pub(super) async fn is_connected_peer(p2p: &Arc<dyn P2POps>, peer_id: &str) -> Result<bool> {
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

pub(super) fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

#[cfg(test)]
pub(super) fn select_local_runtime_pairing_addr(addrs: &[String]) -> Option<String> {
    select_runtime_pairing_addr(addrs, "http://127.0.0.1/")
}

pub(super) fn select_runtime_pairing_addr(addrs: &[String], graphql: &str) -> Option<String> {
    let candidates = addrs
        .iter()
        .map(|addr| addr.trim())
        .filter(|addr| !addr.is_empty())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    let prefer_loopback = graphql_endpoint_is_loopback_or_unspecified(graphql);
    if prefer_loopback {
        candidates
            .iter()
            .find(|addr| addr_has_loopback_hint(addr))
            .map(|addr| (*addr).to_string())
            .or_else(|| candidates.first().map(|addr| (*addr).to_string()))
    } else {
        candidates
            .iter()
            .find(|addr| !addr_has_loopback_hint(addr))
            .map(|addr| (*addr).to_string())
            .or_else(|| candidates.first().map(|addr| (*addr).to_string()))
    }
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

fn graphql_endpoint_is_loopback_or_unspecified(graphql: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(graphql) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|addr| addr.is_loopback() || addr.is_unspecified())
        .unwrap_or(false)
}
