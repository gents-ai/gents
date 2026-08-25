use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime};

use defra_node::EmbeddedNode;
use defra_p2p_adapter::P2POperations as P2POps;
use gents::agent::p2p_reconcile::intervals::endpoint_interval;
#[cfg(test)]
use gents::agent::p2p_reconcile::{
    compute_pairing_diff, DiffOp, PairingActual, PairingDesired, RemoteP2pAdmin,
};
use tokio::sync::{mpsc, watch, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use super::super::peer_directory::{PeerDirectory, PeerRecord};
use super::super::principal_identity::PrincipalIdentity;
use super::bearer_pairing::{
    current_local_endpoint, install_bearer_replicator_for_record, is_bearer_peer,
    observe_bearer_pairing_readiness, publish_local_endpoint,
};
use super::bootstrap::{
    connect_peer_with_retry, force_connect_peer_with_retry, is_connected_peer, request_index_sync,
};
use super::p2p_ops::{
    p2p_connected_peers, p2p_get_replicators, p2p_listen_addresses, p2p_local_peer_id,
    p2p_notify_network_change,
};
use super::route_manager::ClientRouteManager;
use super::route_manager::PendingRemovalCleanup;
use super::{
    ClientPeerStatus, P2PHealth, P2PHealthStatus, P2PSupervisorCommand, P2P_SUPERVISOR_INTERVAL,
    P2P_WEDGED_FAILURE_THRESHOLD,
};

pub(super) fn spawn_p2p_supervisor_task(
    node: Arc<EmbeddedNode>,
    p2p: Arc<dyn P2POps>,
    peer_directory: Arc<RwLock<PeerDirectory>>,
    peer_statuses: Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    p2p_health: watch::Sender<P2PHealth>,
    mut control_rx: mpsc::Receiver<P2PSupervisorCommand>,
    remote_admin_actor: Arc<PrincipalIdentity>,
    route_manager: Arc<ClientRouteManager>,
    install_replicators_on_bootstrap: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut health = p2p_health.borrow().clone();
        let mut ticker = tokio::time::interval(P2P_SUPERVISOR_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let endpoint_refresh_interval = endpoint_interval();
        let mut last_endpoint_refresh = Instant::now() - endpoint_refresh_interval;
        let mut index_requests = BTreeSet::new();
        let mut route_reconciled_at = BTreeMap::new();
        let mut removal_retries = BTreeMap::new();

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
                            target: "gents_desktop_core::p2p_health",
                            "manual desktop P2P repair requested"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "gents_desktop_core::p2p_health",
                            error = %error,
                            "manual desktop P2P repair could not refresh network state"
                        );
                    }
                }
            }

            run_pending_removal_cleanup(
                &peer_directory,
                &peer_statuses,
                &route_manager,
                &mut removal_retries,
            )
            .await;
            run_saved_peer_repair_cycle(
                &node,
                &p2p,
                &peer_directory,
                &peer_statuses,
                &remote_admin_actor,
                &route_manager,
                install_replicators_on_bootstrap,
                manual_repair,
                &mut index_requests,
                &mut route_reconciled_at,
            )
            .await;

            if last_endpoint_refresh.elapsed() >= endpoint_refresh_interval {
                refresh_bearer_endpoint_heartbeat(
                    &node,
                    &p2p,
                    &peer_directory,
                    &remote_admin_actor,
                )
                .await;
                last_endpoint_refresh = Instant::now();
            }

            let next_health = probe_p2p_health(&p2p, &health).await;
            if p2p_health_materially_changed(&health, &next_health) {
                log_p2p_health_transition(&health, &next_health);
                p2p_health.send_replace(next_health.clone());
            }
            health = next_health;
        }
    })
}

async fn run_pending_removal_cleanup(
    peer_directory: &Arc<RwLock<PeerDirectory>>,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    route_manager: &Arc<ClientRouteManager>,
    retries: &mut BTreeMap<String, RemovalRetry>,
) {
    let pending = peer_directory.read().await.pending_removals().to_vec();
    let pending_ids = pending
        .iter()
        .map(|record| record.peer_id.clone())
        .collect::<BTreeSet<_>>();
    retries.retain(|peer_id, _| pending_ids.contains(peer_id));
    for record in pending {
        // A queued removal is already absent from the durable peer directory;
        // never leave an ephemeral status row advertising it while cleanup
        // retries or after the supervisor completes the tombstone.
        if retries
            .get(&record.peer_id)
            .is_some_and(|retry| Instant::now() < retry.retry_after)
        {
            continue;
        }
        let cleanup = route_manager
            .retry_pending_removal(peer_directory, peer_statuses, &record)
            .await;
        match cleanup {
            Ok(PendingRemovalCleanup::Completed | PendingRemovalCleanup::Superseded) => {
                retries.remove(&record.peer_id);
            }
            Err(error) => {
                let error = error.to_string();
                let retry = retries.entry(record.peer_id.clone()).or_default();
                retry.failures = retry.failures.saturating_add(1);
                retry.retry_after = Instant::now() + removal_retry_delay(retry.failures);
                tracing::debug!(
                    directory_peer_id = %record.peer_id,
                    retry_count = retry.failures,
                    error = %error,
                    "pending deployment route teardown will retry with backoff"
                );
            }
        }
    }
}

#[derive(Debug)]
struct RemovalRetry {
    failures: u32,
    retry_after: Instant,
}

impl Default for RemovalRetry {
    fn default() -> Self {
        Self {
            failures: 0,
            retry_after: Instant::now(),
        }
    }
}

fn removal_retry_delay(failures: u32) -> Duration {
    Duration::from_secs(2_u64.saturating_pow(failures.min(5)))
}

async fn refresh_bearer_endpoint_heartbeat(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    peer_directory: &Arc<RwLock<PeerDirectory>>,
    remote_admin_actor: &Arc<PrincipalIdentity>,
) {
    let has_bearer_peer = peer_directory
        .read()
        .await
        .records()
        .iter()
        .any(is_bearer_peer);
    if !has_bearer_peer {
        return;
    }

    match publish_local_endpoint(node.as_ref(), p2p, remote_admin_actor.as_ref()).await {
        Ok(_) => {
            tracing::debug!(
                target: "gents_desktop_core::peer_maintenance",
                "refreshed signed desktop endpoint heartbeat"
            );
        }
        Err(error) => {
            tracing::warn!(
                target: "gents_desktop_core::peer_maintenance",
                error = %error,
                "failed to refresh signed desktop endpoint heartbeat"
            );
        }
    }
}

async fn run_saved_peer_repair_cycle(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    peer_directory: &Arc<RwLock<PeerDirectory>>,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    remote_admin_actor: &Arc<PrincipalIdentity>,
    route_manager: &Arc<ClientRouteManager>,
    install_replicators_on_bootstrap: bool,
    force_repair: bool,
    index_requests: &mut BTreeSet<String>,
    route_reconciled_at: &mut BTreeMap<String, Instant>,
) {
    let records = peer_directory.read().await.records().to_vec();
    let saved_peer_ids = records
        .iter()
        .map(|record| record.peer_id.clone())
        .collect::<BTreeSet<_>>();
    index_requests.retain(|peer_id| saved_peer_ids.contains(peer_id));
    route_reconciled_at.retain(|peer_id, _| saved_peer_ids.contains(peer_id));

    for saved_record in &records {
        let route_lifecycle = route_manager.lock().await;
        if !peer_directory
            .read()
            .await
            .records()
            .iter()
            .any(|current| current == saved_record)
        {
            // The snapshot lost its generation while waiting for the route
            // owner. Never repair or publish status for stale intent.
            continue;
        }
        let route_due = ClientRouteManager::reconcile_due(
            saved_record.pairing_ready,
            route_reconciled_at
                .get(&saved_record.peer_id)
                .map(Instant::elapsed),
            force_repair,
        );
        let record = if route_due {
            route_lifecycle
                .refresh_endpoint(peer_directory, saved_record)
                .await
        } else {
            saved_record.clone()
        };
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
            index_requests.remove(&record.peer_id);
            let updated = repair_saved_peer(
                p2p,
                &record,
                current_status,
                remote_admin_actor.did(),
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

        if still_saved && is_bearer_peer(&record) {
            revalidate_bearer_pairing_readiness(
                node,
                p2p,
                peer_directory,
                peer_statuses,
                remote_admin_actor,
                &record,
            )
            .await;
        }

        if still_saved && !is_bearer_peer(&record) && route_due {
            let reconcile = tokio::time::timeout(
                super::P2P_OPERATION_TIMEOUT,
                route_lifecycle.reconcile(&record, peer_statuses),
            )
            .await;
            match reconcile {
                Ok(Some(route_ready)) => {
                    if let Err(error) = peer_directory
                        .write()
                        .await
                        .set_pairing_ready(&record.peer_id, route_ready)
                        .await
                    {
                        tracing::warn!(
                            target: "gents_desktop_core::pairing_reconcile",
                            directory_peer_id = %record.peer_id,
                            error = %error,
                            "failed to persist client route readiness"
                        );
                    }
                }
                Err(_) => tracing::warn!(
                    target: "gents_desktop_core::pairing_reconcile",
                    directory_peer_id = %record.peer_id,
                    timeout_seconds = super::P2P_OPERATION_TIMEOUT.as_secs(),
                    "client route reconcile timed out; preserving prior readiness until retry"
                ),
                Ok(None) => {}
            }
            route_reconciled_at.insert(record.peer_id.clone(), Instant::now());
        }
    }

    if install_replicators_on_bootstrap {
        request_index_for_ready_peers(node, p2p, peer_statuses, &saved_peer_ids, index_requests)
            .await;
    }
}

pub(super) async fn request_index_for_ready_peers(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    saved_peer_ids: &BTreeSet<String>,
    requested_for: &mut BTreeSet<String>,
) {
    let pending = peer_statuses
        .read()
        .expect("peer status lock poisoned")
        .iter()
        .filter(|status| {
            saved_peer_ids.contains(&status.peer_id)
                && status.dial_succeeded
                && status.last_error.is_none()
                && !requested_for.contains(&status.peer_id)
        })
        .map(|status| status.peer_id.clone())
        .collect::<BTreeSet<_>>();
    if pending.is_empty() {
        return;
    }

    match request_index_sync(node.as_ref(), p2p).await {
        Ok(collections) => {
            requested_for.extend(pending);
            tracing::info!(
                target: "gents_desktop_core::peer_maintenance",
                requested_collections = ?collections,
                "session index sync request dispatched; merges continue asynchronously"
            );
        }
        Err(error) => {
            let message = format!("session index sync request failed: {error}");
            let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
            for status in statuses
                .iter_mut()
                .filter(|status| pending.contains(&status.peer_id))
            {
                status.last_error = Some(message.clone());
            }
            tracing::warn!(
                target: "gents_desktop_core::peer_maintenance",
                error = %error,
                "session index sync request failed; supervisor will retry after repair"
            );
        }
    }
}

async fn revalidate_bearer_pairing_readiness(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    peer_directory: &Arc<RwLock<PeerDirectory>>,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    remote_admin_actor: &Arc<PrincipalIdentity>,
    record: &PeerRecord,
) {
    let result = async {
        let network_id = record
            .pairing_network_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("saved bearer peer has no signed network id"))?;
        let local_endpoint = current_local_endpoint(p2p, remote_admin_actor.as_ref()).await?;
        observe_bearer_pairing_readiness(
            node.as_ref(),
            remote_admin_actor.as_ref(),
            &record.agent_did,
            network_id,
            record.pairing_template.as_deref().unwrap_or("conversation"),
            &local_endpoint,
        )
        .await
    }
    .await;

    let (ready, error) = match result {
        Ok(ready) => (ready, None),
        Err(error) => (
            false,
            Some(format!(
                "peer {} bearer readiness check failed: {}",
                record.label, error
            )),
        ),
    };
    if let Err(error) = peer_directory
        .write()
        .await
        .set_bearer_pairing_ready(&record.peer_id, ready)
        .await
    {
        tracing::warn!(
            target: "gents_desktop_core::peer_maintenance",
            peer_id = %record.peer_id,
            error = %error,
            "failed to persist bearer pairing readiness"
        );
    }

    let mut status = peer_statuses
        .read()
        .expect("peer status lock poisoned")
        .iter()
        .find(|status| status.peer_id == record.peer_id)
        .cloned()
        .unwrap_or_else(|| ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: false,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: record.pairing_ready,
        });
    if let Some(error) = error {
        status.last_error = Some(error);
        replace_peer_status(peer_statuses, status);
    } else if status
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("bearer readiness check failed"))
    {
        status.last_error = None;
        replace_peer_status(peer_statuses, status);
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
            target: "gents_desktop_core::p2p_health",
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
            target: "gents_desktop_core::p2p_health",
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
                target: "gents_desktop_core::peer_maintenance",
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
    requester_did: &str,
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
        routes: Vec::new(),
        chat_safe: record.pairing_ready,
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
                    target: "gents_desktop_core::peer_maintenance",
                    peer_id = %record.peer_id,
                    label = %record.label,
                    "refreshed P2P network state before reconnect"
                );
            }
            Err(error) => {
                tracing::debug!(
                    target: "gents_desktop_core::peer_maintenance",
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
                if install_replicators_on_bootstrap && is_bearer_peer(record) {
                    let replicator_result =
                        install_bearer_replicator_for_record(p2p, record, requester_did).await;
                    if let Err(error) = replicator_result {
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

    status.last_error = None;

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

#[cfg(test)]
mod pairing_reconcile_tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;

    #[test]
    fn pending_removal_retries_use_bounded_exponential_backoff() {
        assert_eq!(removal_retry_delay(1), Duration::from_secs(2));
        assert_eq!(removal_retry_delay(2), Duration::from_secs(4));
        assert_eq!(removal_retry_delay(5), Duration::from_secs(32));
        assert_eq!(removal_retry_delay(50), Duration::from_secs(32));
    }
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
                    filters: Some(Default::default()),
                })
                .collect())
        }

        async fn add_replicator(
            &self,
            addresses: &[String],
            _collections: &[String],
            _filters: &gents::agent::p2p_reconcile::PairingFilters,
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

        async fn resolve_collection_id(&self, name: &str) -> RemoteP2pAdminResult<Option<String>> {
            Ok(Some(name.to_string()))
        }

        async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>> {
            Ok(Some(id.to_string()))
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
            template_ids: Default::default(),
            replicator_collections: ["c1", "c2"].iter().map(|s| s.to_string()).collect(),
            replicator_addresses: ["/ip4/1/p2p/p"].iter().map(|s| s.to_string()).collect(),
            replicator_filter: Default::default(),
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
            ..Default::default()
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
                    stub.add_replicator(
                        &addresses,
                        &[],
                        &gents::agent::p2p_reconcile::PairingFilters::default(),
                    )
                    .await
                    .unwrap();
                }
                DiffOp::TeardownReplicator(address) => {
                    stub.delete_replicator(address, &[]).await.unwrap();
                }
            }
        }
    }
}
