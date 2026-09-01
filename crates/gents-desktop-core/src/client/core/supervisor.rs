use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use defra_node::EmbeddedNode;
use defra_p2p_adapter::P2POperations as P2POps;
#[cfg(test)]
use gents::agent::p2p_reconcile::{
    compute_pairing_diff, DiffOp, PairingActual, PairingDesired, RemoteP2pAdmin,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use super::super::peer_directory::PeerRecord;
use super::super::principal_identity::PrincipalIdentity;
use super::bootstrap::{
    connect_peer_with_retry, force_connect_peer_with_retry, is_connected_peer, request_index_sync,
};
use super::p2p_ops::{
    p2p_connected_peers, p2p_get_replicators, p2p_listen_addresses, p2p_local_peer_id,
    p2p_notify_network_change,
};
use super::route_manager::ClientRouteManager;
use super::route_manager::PendingRemovalCleanup;
use super::sync_state::ClientSyncStateOwner;
use super::{
    ClientPeerStatus, P2PHealth, P2PHealthStatus, P2PSupervisorCommand, P2P_SUPERVISOR_INTERVAL,
    P2P_WEDGED_FAILURE_THRESHOLD,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteReconcileFence {
    record: PeerRecord,
    enrollment: Option<super::enrollment::EnrollmentAuthorizationGeneration>,
}

fn route_reconcile_fence(
    record: &PeerRecord,
    enrollment_authority: &BTreeMap<String, super::enrollment::EnrollmentAuthorizationGeneration>,
) -> RouteReconcileFence {
    RouteReconcileFence {
        record: record.clone(),
        enrollment: (record.source.as_deref() == Some("enrollment"))
            .then(|| enrollment_authority.get(&record.peer_id).cloned())
            .flatten(),
    }
}

pub(super) fn spawn_p2p_supervisor_task(
    node: Arc<EmbeddedNode>,
    p2p: Arc<dyn P2POps>,
    sync_state: ClientSyncStateOwner,
    mut control_rx: mpsc::Receiver<P2PSupervisorCommand>,
    remote_admin_actor: Arc<PrincipalIdentity>,
    local_peer_id: String,
    route_manager: Arc<ClientRouteManager>,
    install_replicators_on_bootstrap: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut health = sync_state.snapshot().transport;
        let mut ticker = tokio::time::interval(P2P_SUPERVISOR_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut index_requests = BTreeMap::new();
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

            let enrollment_authority =
                match super::enrollment::reconcile_status_enrollment_approvals(
                    &node,
                    &p2p,
                    &remote_admin_actor,
                    &local_peer_id,
                    &sync_state,
                    &route_manager,
                )
                .await
                {
                    Ok(authority) => authority,
                    Err(error) => {
                        tracing::warn!(error = %error, "authenticated enrollment observation failed closed");
                        demote_enrollment_readiness_after_authority_failure(&sync_state).await;
                        BTreeMap::new()
                    }
                };
            run_pending_removal_cleanup(&sync_state, &route_manager, &mut removal_retries).await;
            run_saved_peer_repair_cycle(
                &node,
                &p2p,
                &sync_state,
                &remote_admin_actor,
                &route_manager,
                install_replicators_on_bootstrap,
                manual_repair,
                &mut index_requests,
                &mut route_reconciled_at,
                &enrollment_authority,
            )
            .await;

            let next_health = probe_p2p_health(&p2p, &health).await;
            if p2p_health_materially_changed(&health, &next_health) {
                log_p2p_health_transition(&health, &next_health);
                sync_state.replace_transport(next_health.clone());
            }
            health = next_health;
        }
    })
}

async fn demote_enrollment_readiness_after_authority_failure(sync_state: &ClientSyncStateOwner) {
    for record in sync_state
        .records()
        .into_iter()
        .filter(|record| record.source.as_deref() == Some("enrollment") && record.pairing_ready)
    {
        if let Err(persist_error) = sync_state.set_pairing_ready(&record, false).await {
            tracing::warn!(
                peer_id = %record.peer_id,
                error = %persist_error,
                "failed to persist fail-closed enrollment readiness"
            );
        }
    }
}

async fn run_pending_removal_cleanup(
    sync_state: &ClientSyncStateOwner,
    route_manager: &Arc<ClientRouteManager>,
    retries: &mut BTreeMap<String, RemovalRetry>,
) {
    let pending = sync_state.pending_removals().await;
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
            .retry_pending_removal(sync_state, &record)
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

async fn run_saved_peer_repair_cycle(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    sync_state: &ClientSyncStateOwner,
    remote_admin_actor: &Arc<PrincipalIdentity>,
    route_manager: &Arc<ClientRouteManager>,
    install_replicators_on_bootstrap: bool,
    force_repair: bool,
    index_requests: &mut BTreeMap<String, PeerRecord>,
    route_reconciled_at: &mut BTreeMap<String, (RouteReconcileFence, Instant)>,
    enrollment_authority: &BTreeMap<String, super::enrollment::EnrollmentAuthorizationGeneration>,
) {
    let records = sync_state.records();
    let saved_peer_ids = records
        .iter()
        .map(|record| record.peer_id.clone())
        .collect::<BTreeSet<_>>();
    index_requests
        .retain(|peer_id, expected| expected.peer_id == *peer_id && records.contains(expected));
    route_reconciled_at.retain(|peer_id, (expected, _)| {
        expected.record.peer_id == *peer_id
            && records.contains(&expected.record)
            && *expected == route_reconcile_fence(&expected.record, enrollment_authority)
    });

    for saved_record in &records {
        if super::enrollment::enrollment_record_lacks_current_authority(
            saved_record,
            enrollment_authority,
        ) {
            index_requests.remove(&saved_record.peer_id);
            route_reconciled_at.remove(&saved_record.peer_id);
            continue;
        }
        let route_lifecycle = route_manager.lock().await;
        if !sync_state
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
                .filter(|(expected, _)| {
                    *expected == route_reconcile_fence(saved_record, enrollment_authority)
                })
                .map(|(_, reconciled_at)| reconciled_at.elapsed()),
            force_repair,
        );
        let record = if route_due {
            route_lifecycle
                .refresh_endpoint(sync_state, saved_record)
                .await
        } else {
            saved_record.clone()
        };
        let current_status = sync_state.peer(&record.peer_id);

        let needs_repair =
            force_repair || saved_peer_needs_repair(p2p, &record, current_status.as_ref()).await;

        let mut still_saved = sync_state
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
            still_saved = sync_state
                .records()
                .iter()
                .any(|candidate| candidate.peer_id == record.peer_id);
            if still_saved {
                sync_state.replace_peer(&record, updated);
            }
        }

        if still_saved && install_replicators_on_bootstrap && route_due {
            let reconcile = tokio::time::timeout(
                super::P2P_OPERATION_TIMEOUT,
                route_lifecycle.reconcile(
                    &record,
                    sync_state,
                    enrollment_authority.contains_key(&record.peer_id),
                ),
            )
            .await;
            match reconcile {
                Ok(Some(route_ready)) => {
                    match persist_pairing_readiness(sync_state, &record, route_ready).await {
                        Ok(Some(_)) => {}
                        Ok(None) => {}
                        Err(error) => tracing::warn!(
                            target: "gents_desktop_core::pairing_reconcile",
                            directory_peer_id = %record.peer_id,
                            error = %error,
                            "failed to persist client route readiness"
                        ),
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
            route_reconciled_at.insert(
                record.peer_id.clone(),
                (
                    route_reconcile_fence(&record, enrollment_authority),
                    Instant::now(),
                ),
            );
        }
    }

    if install_replicators_on_bootstrap {
        request_index_for_ready_peers(node, p2p, sync_state, &saved_peer_ids, index_requests).await;
    }
}

pub(super) async fn request_index_for_ready_peers(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    sync_state: &ClientSyncStateOwner,
    saved_peer_ids: &BTreeSet<String>,
    requested_for: &mut BTreeMap<String, PeerRecord>,
) {
    let snapshot = sync_state.snapshot();
    let pending = snapshot
        .directory
        .iter()
        .filter(|record| {
            snapshot
                .peers
                .iter()
                .find(|status| status.peer_id == record.peer_id)
                .is_some_and(|status| {
                    saved_peer_ids.contains(&record.peer_id)
                        && status.dial_succeeded
                        && status.last_error.is_none()
                        && requested_for.get(&record.peer_id) != Some(record)
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return;
    }

    match request_index_sync(node.as_ref(), p2p).await {
        Ok(collections) => {
            let current = sync_state.records();
            requested_for.extend(
                pending
                    .iter()
                    .filter(|expected| current.contains(expected))
                    .map(|record| (record.peer_id.clone(), record.clone())),
            );
            tracing::info!(
                target: "gents_desktop_core::peer_maintenance",
                requested_collections = ?collections,
                "session index sync request dispatched; merges continue asynchronously"
            );
        }
        Err(error) => {
            let message = format!("session index sync request failed: {error}");
            sync_state.set_last_error_for_records(&pending, message);
            tracing::warn!(
                target: "gents_desktop_core::peer_maintenance",
                error = %error,
                "session index sync request failed; supervisor will retry after repair"
            );
        }
    }
}

/// Persist readiness before publishing the configured-peer revision that
/// wakes bridge projections. A failed directory write never becomes an
/// optimistic UI observation.
async fn persist_pairing_readiness(
    sync_state: &ClientSyncStateOwner,
    expected: &PeerRecord,
    ready: bool,
) -> anyhow::Result<Option<PeerRecord>> {
    sync_state.set_pairing_ready(expected, ready).await
}

#[cfg(test)]
fn status_for_record(record: &PeerRecord) -> ClientPeerStatus {
    ClientPeerStatus {
        peer_id: record.peer_id.clone(),
        label: record.label.clone(),
        agent_did: record.agent_did.clone(),
        addr: record.addr.clone(),
        dial_succeeded: false,
        last_error: None,
        pairing: Vec::new(),
        routes: Vec::new(),
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
    _requester_did: &str,
    _install_replicators_on_bootstrap: bool,
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

#[cfg(test)]
mod pairing_reconcile_tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::time::Duration;

    use crate::client::PeerDirectory;
    use async_trait::async_trait;

    use super::*;

    async fn assert_persisted_readiness_wakes_snapshot(source: &str) {
        let tempdir = tempfile::tempdir().expect("temporary peer directory");
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::open_writer(&path)
            .await
            .expect("load directory");
        let mut record = PeerRecord::new("Mandrake", "endpoint", "did:key:mandrake");
        record.source = Some(source.to_string());
        directory
            .upsert(record.clone())
            .await
            .expect("persist peer");
        let status = status_for_record(&record);
        let owner = ClientSyncStateOwner::new(P2PHealth::default(), directory, vec![status]);
        let mut updates = owner.subscribe();

        for ready in [true, false] {
            let expected = owner.records().into_iter().next().expect("configured peer");
            persist_pairing_readiness(&owner, &expected, ready)
                .await
                .expect("persist readiness")
                .expect("configured peer remains present");

            updates.changed().await.expect("sync watch remains open");
            let snapshot = updates.borrow_and_update().clone();
            assert_eq!(snapshot.directory.len(), 1);
            assert_eq!(snapshot.peers.len(), 1);
            assert_eq!(snapshot.directory[0].peer_id, snapshot.peers[0].peer_id);
            assert_eq!(snapshot.directory[0].pairing_ready, ready);
            let reloaded = crate::client::load_peer_records(&path)
                .await
                .expect("reload directory");
            assert_eq!(reloaded[0].pairing_ready, ready);
        }
    }

    #[tokio::test]
    async fn managed_readiness_true_and_false_publish_persisted_snapshot_revision() {
        assert_persisted_readiness_wakes_snapshot("enrollment").await;
    }

    #[tokio::test]
    async fn authority_failure_demotes_enrollment_and_preserves_other_peers() {
        let tempdir = tempfile::tempdir().expect("temporary peer directory");
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::open_writer(&path)
            .await
            .expect("load directory");
        let mut enrollment = PeerRecord::new("Enrollment", "endpoint-a", "did:key:enrolled");
        enrollment.source = Some("enrollment".to_string());
        enrollment.pairing_ready = true;
        let mut direct = PeerRecord::new("Direct", "endpoint-b", "did:key:direct");
        direct.source = Some("local-standard".to_string());
        direct.pairing_ready = true;
        directory
            .upsert(enrollment.clone())
            .await
            .expect("persist enrollment peer");
        directory
            .upsert(direct.clone())
            .await
            .expect("persist direct peer");
        let owner = ClientSyncStateOwner::new(
            P2PHealth::default(),
            directory,
            vec![status_for_record(&enrollment), status_for_record(&direct)],
        );

        demote_enrollment_readiness_after_authority_failure(&owner).await;

        let records = owner.records();
        assert!(
            !records
                .iter()
                .find(|record| record.peer_id == enrollment.peer_id)
                .expect("enrollment peer")
                .pairing_ready
        );
        assert!(
            records
                .iter()
                .find(|record| record.peer_id == direct.peer_id)
                .expect("direct peer")
                .pairing_ready
        );
        let persisted = crate::client::load_peer_records(&path)
            .await
            .expect("reload directory");
        assert!(
            !persisted
                .iter()
                .find(|record| record.peer_id == enrollment.peer_id)
                .expect("persisted enrollment peer")
                .pairing_ready
        );
    }

    #[test]
    fn pending_removal_retries_use_bounded_exponential_backoff() {
        assert_eq!(removal_retry_delay(1), Duration::from_secs(2));
        assert_eq!(removal_retry_delay(2), Duration::from_secs(4));
        assert_eq!(removal_retry_delay(5), Duration::from_secs(32));
        assert_eq!(removal_retry_delay(50), Duration::from_secs(32));
    }

    #[test]
    fn enrollment_route_cache_is_scoped_to_exact_authorization_generation() {
        let mut record = PeerRecord::new("Enrollment", "endpoint", "did:key:server");
        record.source = Some("enrollment".to_string());
        let mut authority = BTreeMap::from([(
            record.peer_id.clone(),
            super::super::enrollment::EnrollmentAuthorizationGeneration {
                request_digest: "request-a".to_string(),
                sequence: 1,
                expires_at: "2099-09-29T00:00:00Z".into(),
            },
        )]);
        let first = route_reconcile_fence(&record, &authority);

        authority.insert(
            record.peer_id.clone(),
            super::super::enrollment::EnrollmentAuthorizationGeneration {
                request_digest: "request-b".to_string(),
                sequence: 1,
                expires_at: "2099-09-29T00:00:00Z".into(),
            },
        );
        assert_ne!(first, route_reconcile_fence(&record, &authority));

        authority.insert(
            record.peer_id.clone(),
            super::super::enrollment::EnrollmentAuthorizationGeneration {
                request_digest: "request-a".to_string(),
                sequence: 2,
                expires_at: "2099-10-29T00:00:00Z".into(),
            },
        );
        assert_ne!(first, route_reconcile_fence(&record, &authority));
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
