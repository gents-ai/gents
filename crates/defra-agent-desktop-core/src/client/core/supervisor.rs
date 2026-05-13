use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::SystemTime;

use defra_node::EmbeddedNode;
use defra_p2p_adapter::P2POperations as P2POps;
use tokio::sync::{mpsc, watch, RwLock};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::super::peer_directory::{PeerDirectory, PeerRecord};
use super::super::schema::subscribed_collection_names;
use super::bootstrap::{
    add_replicator_with_retry, configure_local_runtime_pairing_legacy, connect_peer_with_retry,
    force_connect_peer_with_retry, is_connected_peer, p2p_pairing_enabled_for_graphql,
    pairing_reconcile_enabled, REMOTE_P2P_PAIRING_ENV,
};
use super::p2p_ops::{
    p2p_connected_peers, p2p_get_replicators, p2p_listen_addresses, p2p_local_peer_id,
    p2p_notify_network_change,
};
use super::{
    ClientPeerStatus, P2PHealth, P2PHealthStatus, P2PSupervisorCommand, PairingCollectionStatus,
    P2P_SUPERVISOR_INTERVAL, P2P_WEDGED_FAILURE_THRESHOLD,
};
use crate::remote_admin::{
    classify_remote_admin_error, compute_pairing_diff, DiffOp, HttpRemoteP2pAdmin, PairingActual,
    PairingDesired, RemoteP2pAdmin,
};

pub(super) fn spawn_p2p_supervisor_task(
    node: Arc<EmbeddedNode>,
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
                            target: "defra_agent_desktop_core::p2p_health",
                            "manual desktop P2P repair requested"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "defra_agent_desktop_core::p2p_health",
                            error = %error,
                            "manual desktop P2P repair could not refresh network state"
                        );
                    }
                }
            }

            run_saved_peer_repair_cycle(
                &node,
                &p2p,
                &peer_directory,
                &peer_statuses,
                install_replicators_on_bootstrap,
                manual_repair,
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
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    peer_directory: &Arc<RwLock<PeerDirectory>>,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    install_replicators_on_bootstrap: bool,
    force_repair: bool,
) {
    let records = peer_directory.read().await.records().to_vec();
    for record in records {
        let current_status = peer_statuses
            .read()
            .expect("peer status lock poisoned")
            .iter()
            .find(|status| status.peer_id == record.peer_id)
            .cloned();

        let needs_repair =
            force_repair || saved_peer_needs_repair(p2p, &record, current_status.as_ref()).await;

        let mut still_saved = peer_directory
            .read()
            .await
            .records()
            .iter()
            .any(|candidate| candidate.peer_id == record.peer_id);
        if needs_repair {
            let updated = repair_saved_peer(
                p2p,
                &record,
                current_status,
                install_replicators_on_bootstrap,
                force_repair,
            )
            .await;
            still_saved = peer_directory
                .read()
                .await
                .records()
                .iter()
                .any(|candidate| candidate.peer_id == record.peer_id);
            if still_saved {
                replace_peer_status(peer_statuses, updated);
            }
        }

        if still_saved && pairing_reconcile_enabled() {
            let desired = load_desired_for_peer(node, &record).await;
            run_pairing_reconcile_for_peer(&record, desired, peer_statuses).await;
        }
    }
}

pub(super) async fn probe_p2p_health(p2p: &Arc<dyn P2POps>, previous: &P2PHealth) -> P2PHealth {
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
            target: "defra_agent_desktop_core::p2p_health",
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
            target: "defra_agent_desktop_core::p2p_health",
            status,
            consecutive_failures = next.consecutive_failures,
            error,
            "desktop P2P transport health degraded"
        );
    }
}

pub(super) fn p2p_health_materially_changed(previous: &P2PHealth, next: &P2PHealth) -> bool {
    previous.status != next.status
        || previous.consecutive_failures != next.consecutive_failures
        || previous.connected_peer_count != next.connected_peer_count
        || previous.replicator_count != next.replicator_count
        || previous.last_error != next.last_error
}

pub(super) async fn saved_peer_needs_repair(
    p2p: &Arc<dyn P2POps>,
    record: &PeerRecord,
    status: Option<&ClientPeerStatus>,
) -> bool {
    if status.is_none()
        || status.is_some_and(|status| !status.dial_succeeded || status.last_error.is_some())
    {
        return true;
    }

    let Some(expected_peer_id) = p2p::iroh::parse_public_peer_addr(&record.addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
    else {
        return false;
    };

    match is_connected_peer(p2p, &expected_peer_id).await {
        Ok(connected) => !connected,
        Err(error) => {
            tracing::debug!(
                target: "defra_agent_desktop_core::peer_maintenance",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %error,
                "failed to check live P2P connectivity; forcing repair"
            );
            true
        }
    }
}

pub(super) async fn repair_saved_peer(
    p2p: &Arc<dyn P2POps>,
    record: &PeerRecord,
    current_status: Option<ClientPeerStatus>,
    install_replicators_on_bootstrap: bool,
    force_repair: bool,
) -> ClientPeerStatus {
    let mut status = current_status.unwrap_or_else(|| ClientPeerStatus {
        peer_id: record.peer_id.clone(),
        label: record.label.clone(),
        agent_did: record.agent_did.clone(),
        addr: record.addr.clone(),
        dial_succeeded: false,
        last_error: None,
        pairing: Vec::new(),
    });

    let expected_peer_id = p2p::iroh::parse_public_peer_addr(&record.addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string());
    let connected_now = match expected_peer_id.as_deref() {
        Some(peer_id) => is_connected_peer(p2p, peer_id).await.unwrap_or(false),
        None => status.dial_succeeded,
    };

    if force_repair || !connected_now {
        match p2p_notify_network_change(p2p).await {
            Ok(()) => {
                tracing::debug!(
                    target: "defra_agent_desktop_core::peer_maintenance",
                    peer_id = %record.peer_id,
                    label = %record.label,
                    "refreshed P2P network state before reconnect"
                );
            }
            Err(error) => {
                tracing::debug!(
                    target: "defra_agent_desktop_core::peer_maintenance",
                    peer_id = %record.peer_id,
                    label = %record.label,
                    error = %error,
                    "failed to refresh P2P network state before reconnect"
                );
            }
        }

        let connect_result = if force_repair {
            force_connect_peer_with_retry(p2p, &record.addr, &record.label).await
        } else {
            connect_peer_with_retry(p2p, &record.addr, &record.label).await
        };

        match connect_result {
            Ok(()) => {
                status.dial_succeeded = true;
                let p2p_pairing_enabled = record
                    .graphql
                    .as_deref()
                    .map(p2p_pairing_enabled_for_graphql)
                    .unwrap_or(true);
                if install_replicators_on_bootstrap && p2p_pairing_enabled {
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
                } else if record.graphql.is_some() && !p2p_pairing_enabled {
                    tracing::debug!(
                        target: "defra_agent_desktop_core::peer_maintenance",
                        peer_id = %record.peer_id,
                        label = %record.label,
                        env = REMOTE_P2P_PAIRING_ENV,
                        "skipping automatic remote P2P replicator repair for GraphQL-managed peer"
                    );
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
        if pairing_reconcile_enabled() {
            status.last_error = None;
        } else if p2p_pairing_enabled_for_graphql(graphql) {
            match configure_local_runtime_pairing_legacy(p2p, graphql).await {
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
    } else {
        status.last_error = None;
    }

    status
}

async fn load_desired_for_peer(node: &Arc<EmbeddedNode>, record: &PeerRecord) -> PairingDesired {
    use defra_agent_protocol::graphql::escape_graphql_string;

    let peer_id = escape_graphql_string(&record.peer_id);
    let query = format!(
        r#"query {{ PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{ collections replicator_addresses }} }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        tracing::warn!(
            target: "defra_agent_desktop_core::pairing_reconcile",
            peer_id = %record.peer_id,
            label = %record.label,
            errors = ?response.errors,
            "PeerPairingDesired query failed; using empty desired state"
        );
        return PairingDesired::default();
    }

    let Some(row) = response
        .data
        .as_ref()
        .and_then(|data| data.get("PeerPairingDesired"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
    else {
        return PairingDesired::default();
    };

    let collections = row
        .get("collections")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let replicator_addresses = row
        .get("replicator_addresses")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    PairingDesired {
        collections,
        replicator_addresses,
    }
}

async fn run_pairing_reconcile_for_peer(
    record: &PeerRecord,
    desired: PairingDesired,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
) {
    let Some(graphql_url) = record.graphql.as_deref() else {
        return;
    };
    let admin = match HttpRemoteP2pAdmin::new(graphql_url) {
        Ok(admin) => admin,
        Err(error) => {
            tracing::warn!(
                target: "defra_agent_desktop_core::pairing_reconcile",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %error,
                "failed to construct remote P2P admin"
            );
            return;
        }
    };

    let actual_collections = match admin.list_p2p_collections().await {
        Ok(collections) => collections.into_iter().collect(),
        Err(error) => {
            record_failure(record, peer_statuses, &desired, &error);
            return;
        }
    };
    let remote_replicators = match admin.list_replicators().await {
        Ok(replicators) => replicators,
        Err(error) => {
            record_failure(record, peer_statuses, &desired, &error);
            return;
        }
    };
    let actual_replicators = remote_replicators
        .iter()
        .filter_map(|replicator| replicator.address.clone())
        .collect();
    let replicator_ids_by_addr = remote_replicators
        .iter()
        .filter_map(|replicator| {
            Some((
                replicator.address.as_ref()?.clone(),
                replicator.id.as_ref()?.clone(),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let actual = PairingActual {
        collections: actual_collections,
        replicator_addresses: actual_replicators,
    };
    let ops = compute_pairing_diff(&desired, &actual);
    let replicator_collections = desired.collections.iter().cloned().collect::<Vec<_>>();

    for op in &ops {
        let result = match op {
            DiffOp::InstallCollection(collection) => {
                let collections = vec![collection.clone()];
                admin.add_p2p_collections(&collections).await
            }
            DiffOp::TeardownCollection(collection) => {
                let collections = vec![collection.clone()];
                admin.delete_p2p_collections(&collections).await
            }
            DiffOp::InstallReplicator(address) => {
                let addresses = vec![address.clone()];
                admin
                    .add_replicator(&addresses, &replicator_collections)
                    .await
            }
            DiffOp::TeardownReplicator(address) => {
                let id = replicator_ids_by_addr
                    .get(address)
                    .map(String::as_str)
                    .unwrap_or(address.as_str());
                admin.delete_replicator(id, &replicator_collections).await
            }
        };

        match result {
            Ok(()) => record_success_for_op(record, peer_statuses, &desired, op),
            Err(error) => {
                record_failure_for_op(record, peer_statuses, &desired, op, &error);
                break;
            }
        }
    }
}

fn record_failure(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    desired: &PairingDesired,
    err: &crate::remote_admin::RemoteP2pAdminError,
) {
    let class = classify_remote_admin_error(err);
    let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
    let Some(status) = statuses
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    else {
        return;
    };

    for collection_id in desired.collections.iter() {
        let sub = ensure_pairing_status(status, collection_id);
        sub.record_retry(class);
        sub.update_stuck_indicator(SystemTime::now());
    }
}

fn record_success_for_op(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    desired: &PairingDesired,
    op: &DiffOp,
) {
    let targets = op_status_targets(op, desired);
    let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
    let Some(status) = statuses
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    else {
        return;
    };

    for target in targets {
        ensure_pairing_status(status, &target).record_success();
    }
}

fn record_failure_for_op(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    desired: &PairingDesired,
    op: &DiffOp,
    err: &crate::remote_admin::RemoteP2pAdminError,
) {
    let class = classify_remote_admin_error(err);
    let targets = op_status_targets(op, desired);
    let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
    let Some(status) = statuses
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    else {
        return;
    };

    for target in targets {
        let sub = ensure_pairing_status(status, &target);
        sub.record_retry(class);
        sub.update_stuck_indicator(SystemTime::now());
    }
}

fn ensure_pairing_status<'a>(
    status: &'a mut ClientPeerStatus,
    collection_id: &str,
) -> &'a mut PairingCollectionStatus {
    if let Some(pos) = status
        .pairing
        .iter()
        .position(|existing| existing.collection_id == collection_id)
    {
        &mut status.pairing[pos]
    } else {
        status
            .pairing
            .push(PairingCollectionStatus::new(collection_id));
        status.pairing.last_mut().expect("pairing status inserted")
    }
}

fn op_status_targets(op: &DiffOp, desired: &PairingDesired) -> Vec<String> {
    match op {
        DiffOp::InstallCollection(collection) | DiffOp::TeardownCollection(collection) => {
            vec![collection.clone()]
        }
        DiffOp::InstallReplicator(_) | DiffOp::TeardownReplicator(_) => {
            desired.collections.iter().cloned().collect()
        }
    }
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

#[cfg(test)]
mod pairing_reconcile_tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::remote_admin::{RemoteP2pAdminResult, RemoteReplicator};

    struct StubRemoteAdmin {
        installed_collections: Mutex<BTreeSet<String>>,
        installed_replicators: Mutex<BTreeSet<String>>,
        emitted: Mutex<Vec<DiffOp>>,
    }

    impl StubRemoteAdmin {
        fn new() -> Self {
            Self {
                installed_collections: Mutex::new(BTreeSet::new()),
                installed_replicators: Mutex::new(BTreeSet::new()),
                emitted: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl RemoteP2pAdmin for StubRemoteAdmin {
        async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(vec![])
        }

        async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(vec![])
        }

        async fn connect(&self, _addresses: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
            let replicators = self.installed_replicators.lock().unwrap();
            Ok(replicators
                .iter()
                .map(|addr| RemoteReplicator {
                    id: None,
                    collections: vec![],
                    address: Some(addr.clone()),
                })
                .collect())
        }

        async fn add_replicator(
            &self,
            addresses: &[String],
            _collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            for address in addresses {
                self.installed_replicators
                    .lock()
                    .unwrap()
                    .insert(address.clone());
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::InstallReplicator(address.clone()));
            }
            Ok(())
        }

        async fn delete_replicator(
            &self,
            id: &str,
            _collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            self.installed_replicators.lock().unwrap().remove(id);
            self.emitted
                .lock()
                .unwrap()
                .push(DiffOp::TeardownReplicator(id.to_string()));
            Ok(())
        }

        async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(self
                .installed_collections
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect())
        }

        async fn add_p2p_collections(&self, cols: &[String]) -> RemoteP2pAdminResult<()> {
            for collection in cols {
                self.installed_collections
                    .lock()
                    .unwrap()
                    .insert(collection.clone());
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::InstallCollection(collection.clone()));
            }
            Ok(())
        }

        async fn delete_p2p_collections(&self, cols: &[String]) -> RemoteP2pAdminResult<()> {
            for collection in cols {
                self.installed_collections
                    .lock()
                    .unwrap()
                    .remove(collection);
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::TeardownCollection(collection.clone()));
            }
            Ok(())
        }

        async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(vec![])
        }

        async fn add_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn delete_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn sync_documents(
            &self,
            _collection_name: &str,
            _doc_ids: &[String],
            _timeout: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn sync_collection_versions(
            &self,
            _version_ids: &[String],
            _timeout: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn sync_branchable_collection(
            &self,
            _collection_id: &str,
            _timeout: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn diff_drives_install_and_no_op_after_convergence() {
        let stub = StubRemoteAdmin::new();
        let desired = PairingDesired {
            collections: ["c1", "c2"].iter().map(|s| s.to_string()).collect(),
            replicator_addresses: ["/ip4/1/p2p/p"].iter().map(|s| s.to_string()).collect(),
        };

        let actual_1 = read_remote_actual(&stub).await;
        let ops_1 = compute_pairing_diff(&desired, &actual_1);
        apply_ops(&stub, &ops_1).await;
        assert_eq!(ops_1.len(), 3);

        let actual_2 = read_remote_actual(&stub).await;
        let ops_2 = compute_pairing_diff(&desired, &actual_2);
        assert!(ops_2.is_empty());
    }

    async fn read_remote_actual(stub: &StubRemoteAdmin) -> PairingActual {
        let collections = stub.list_p2p_collections().await.unwrap();
        let replicators = stub.list_replicators().await.unwrap();
        PairingActual {
            collections: collections.into_iter().collect(),
            replicator_addresses: replicators
                .into_iter()
                .filter_map(|replicator| replicator.address)
                .collect(),
        }
    }

    async fn apply_ops(stub: &StubRemoteAdmin, ops: &[DiffOp]) {
        for op in ops {
            match op {
                DiffOp::InstallCollection(collection) => {
                    let collections = vec![collection.clone()];
                    stub.add_p2p_collections(&collections).await.unwrap();
                }
                DiffOp::TeardownCollection(collection) => {
                    let collections = vec![collection.clone()];
                    stub.delete_p2p_collections(&collections).await.unwrap();
                }
                DiffOp::InstallReplicator(address) => {
                    let addresses = vec![address.clone()];
                    stub.add_replicator(&addresses, &[]).await.unwrap();
                }
                DiffOp::TeardownReplicator(address) => {
                    stub.delete_replicator(address, &[]).await.unwrap();
                }
            }
        }
    }
}
