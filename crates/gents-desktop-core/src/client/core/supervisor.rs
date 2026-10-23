use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use defra_node::EmbeddedNode;
use defra_p2p_adapter::P2POperations as P2POps;
use gents::{JsonP2pSyncStatusAdapter, P2pSyncStatusAdapter, P2pSyncStatusSnapshot};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use super::super::peer_directory::PeerRecord;
use super::super::principal_identity::PrincipalIdentity;
use super::bootstrap::{connect_peer_with_retry, force_connect_peer_with_retry, is_connected_peer};
use super::p2p_ops::{
    p2p_connected_peers, p2p_get_replicators, p2p_listen_addresses, p2p_local_peer_id,
    p2p_notify_network_change, p2p_sync_status,
};
use super::route_manager::ClientRouteManager;
use super::route_manager::PendingRemovalCleanup;
use super::sync_state::ClientSyncStateOwner;
use super::{
    p2p_health_materially_changed, ClientPeerStatus, P2PHealth, P2PHealthStatus,
    P2PSupervisorCommand, P2P_SUPERVISOR_INTERVAL, P2P_WEDGED_FAILURE_THRESHOLD,
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

#[expect(
    clippy::too_many_arguments,
    reason = "the supervisor task owns each handle for its lifetime"
)]
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
        let mut ticker = tokio::time::interval(P2P_SUPERVISOR_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
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
                node.event_bus().as_ref(),
                &p2p,
                &sync_state,
                &remote_admin_actor,
                &route_manager,
                install_replicators_on_bootstrap,
                manual_repair,
                &mut route_reconciled_at,
                &enrollment_authority,
            )
            .await;

            let previous = sync_state.snapshot();
            let (next_health, next_database_sync, next_database_sync_error) = probe_p2p_health(
                &p2p,
                &previous.transport,
                previous.database_sync,
                previous.database_sync_error,
            )
            .await;
            if p2p_health_materially_changed(&previous.transport, &next_health) {
                log_p2p_health_transition(&previous.transport, &next_health);
            }
            sync_state.replace_database_observation(
                next_health,
                next_database_sync,
                next_database_sync_error,
            );
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

#[expect(
    clippy::too_many_arguments,
    reason = "supervisor state borrowed from separate owners"
)]
async fn run_saved_peer_repair_cycle(
    bus: &dyn events::Bus,
    p2p: &Arc<dyn P2POps>,
    sync_state: &ClientSyncStateOwner,
    remote_admin_actor: &Arc<PrincipalIdentity>,
    route_manager: &Arc<ClientRouteManager>,
    install_replicators_on_bootstrap: bool,
    force_repair: bool,
    route_reconciled_at: &mut BTreeMap<String, (RouteReconcileFence, Instant)>,
    enrollment_authority: &BTreeMap<String, super::enrollment::EnrollmentAuthorizationGeneration>,
) {
    let records = sync_state.records();
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
            route_reconciled_at.remove(&saved_record.peer_id);
            continue;
        }
        let route_lifecycle = route_manager.lock().await;
        if !sync_state
            .records()
            .iter()
            .any(|current| current == saved_record)
        {
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
            let updated = repair_saved_peer(
                bus,
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
}

/// Persist readiness before publishing the configured-peer revision.
async fn persist_pairing_readiness(
    sync_state: &ClientSyncStateOwner,
    expected: &PeerRecord,
    ready: bool,
) -> anyhow::Result<Option<PeerRecord>> {
    sync_state.set_pairing_ready(expected, ready).await
}

pub(super) async fn probe_p2p_health(
    p2p: &Arc<dyn P2POps>,
    previous: &P2PHealth,
    previous_database_sync: Option<P2pSyncStatusSnapshot>,
    previous_database_sync_error: Option<String>,
) -> (P2PHealth, Option<P2pSyncStatusSnapshot>, Option<String>) {
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
        Ok((connected_peer_count, replicator_count)) => {
            let (database_sync, database_sync_error) = match p2p_sync_status(p2p).await {
                Ok(sync_status) if sync_status.is_null() => (None, None),
                Ok(sync_status) => match JsonP2pSyncStatusAdapter.adapt(&sync_status) {
                    Ok(status) => (Some(status), None),
                    Err(error) => (None, Some(format!("invalid database sync status: {error}"))),
                },
                Err(error) => (
                    None,
                    Some(format!("failed to read database sync status: {error}")),
                ),
            };
            (
                P2PHealth {
                    status: P2PHealthStatus::Healthy,
                    consecutive_failures: 0,
                    connected_peer_count,
                    replicator_count,
                    last_error: None,
                    last_ok_at: Some(now),
                    last_failure_at: previous.last_failure_at,
                },
                database_sync,
                database_sync_error,
            )
        }
        Err(error) => {
            let consecutive_failures = previous.consecutive_failures.saturating_add(1);
            let status = if consecutive_failures >= P2P_WEDGED_FAILURE_THRESHOLD {
                P2PHealthStatus::Wedged
            } else {
                P2PHealthStatus::Degraded
            };
            (
                P2PHealth {
                    status,
                    consecutive_failures,
                    connected_peer_count: previous.connected_peer_count,
                    replicator_count: previous.replicator_count,
                    last_error: Some(error.to_string()),
                    last_ok_at: previous.last_ok_at,
                    last_failure_at: Some(now),
                },
                previous_database_sync,
                previous_database_sync_error,
            )
        }
    }
}

fn log_p2p_health_transition(previous: &P2PHealth, next: &P2PHealth) {
    if next.status == P2PHealthStatus::Healthy {
        tracing::info!(
            target: "gents_desktop_core::p2p_health",
            connected_peers = next.connected_peer_count,
            replicators = next.replicator_count,
            "desktop P2P transport probe succeeded"
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
    bus: &dyn events::Bus,
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
        node_did: record.node_did.clone(),
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
            force_connect_peer_with_retry(bus, p2p, &record.addr, &record.label).await
        } else {
            connect_peer_with_retry(bus, p2p, &record.addr, &record.label).await
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
