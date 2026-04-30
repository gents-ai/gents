use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::SystemTime;

use defra_p2p_adapter::P2POperations as P2POps;
use tokio::sync::{mpsc, watch, RwLock};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::super::peer_directory::{PeerDirectory, PeerRecord};
use super::super::schema::subscribed_collection_names;
use super::bootstrap::{
    add_replicator_with_retry, configure_local_runtime_pairing, connect_peer_with_retry,
    force_connect_peer_with_retry, is_connected_peer, p2p_pairing_enabled_for_graphql,
    REMOTE_P2P_PAIRING_ENV,
};
use super::p2p_ops::{
    p2p_connected_peers, p2p_get_replicators, p2p_listen_addresses, p2p_local_peer_id,
    p2p_notify_network_change,
};
use super::{
    ClientPeerStatus, P2PHealth, P2PHealthStatus, P2PSupervisorCommand, P2P_SUPERVISOR_INTERVAL,
    P2P_WEDGED_FAILURE_THRESHOLD,
};

pub(super) fn spawn_p2p_supervisor_task(
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

        if !force_repair && !saved_peer_needs_repair(p2p, &record, current_status.as_ref()).await {
            continue;
        }

        let updated = repair_saved_peer(
            p2p,
            &record,
            current_status,
            install_replicators_on_bootstrap,
            force_repair,
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
        if p2p_pairing_enabled_for_graphql(graphql) {
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
