use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use futures::{stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::store::identity::NodeIdentity;

use super::super::enrollment_reconcile::EnrollmentAuthorityHandle;
use super::super::{EmbeddedRemoteP2pAdmin, RemoteP2pAdmin};
use super::reconcile::{prepare_pairing_peer, reconcile_prepared_peer};
use super::{GraphqlPairingStateStore, PairingStateStore, MAX_CONCURRENT_PEER_PREPARATIONS};

const PAIRING_STATE_COLLECTIONS: [&str; 3] = [
    "PeerPairingDesired",
    "DataPlanePairingDesired",
    "PeerPairingApplied",
];

pub async fn run_pairing_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    enrollment: EnrollmentAuthorityHandle,
    cancel: CancellationToken,
) -> Result<()> {
    if node.p2p_arc().is_none() {
        tracing::debug!(target: "gents::agent::p2p_reconcile::engine", "pairing reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    }

    let admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let store =
        GraphqlPairingStateStore::with_enrollment_authority(node.clone(), identity, enrollment);
    let subscription = crate::subscribe_collections(&node, PAIRING_STATE_COLLECTIONS)?;
    let connections = node.subscribe(&[events::EventName::PeerConnection]);

    run_pairing_reconciler_loop(&admin, &store, subscription, connections, &cancel).await;
    Ok(())
}

pub(super) async fn run_pairing_reconciler_loop(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    mut subscription: events::DocumentChangeSubscription,
    mut connections: events::Subscription,
    cancel: &CancellationToken,
) {
    let mut interval = tokio::time::interval(super::super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut replay_connections = BTreeMap::new();
    let mut failing_peers = BTreeSet::<String>::new();
    let mut subscription_open = true;
    let mut dropped = BTreeSet::<String>::new();
    let mut connections_open = true;

    if !sweep_pairings_logged_until_cancelled(
        admin,
        store,
        &mut replay_connections,
        &mut failing_peers,
        &mut dropped,
        cancel,
    )
    .await
    {
        return;
    }

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = interval.tick() => {
                if !sweep_pairings_logged_until_cancelled(
                    admin,
                    store,
                    &mut replay_connections,
                    &mut failing_peers,
                    &mut dropped,
                    cancel,
                ).await {
                    return;
                }
            }
            message = connections.recv(), if connections_open => {
                let Some(message) = message else {
                    tracing::warn!(target: "gents::agent::p2p_reconcile::engine", "pairing reconciler peer-connection subscription closed; reconnects wait for periodic sweeps");
                    connections_open = false;
                    continue;
                };
                let mut reconnected = record_peer_connection(&message, &mut dropped);
                while let Ok(message) = connections.try_recv() {
                    reconnected |= record_peer_connection(&message, &mut dropped);
                }
                if connections.check_and_reset_dropped() > 0 {
                    // A lost disconnect cannot be attributed to a peer.
                    replay_connections.values_mut().for_each(|active| *active = false);
                    reconnected = true;
                }
                if !reconnected {
                    continue;
                }
                if !sweep_pairings_logged_until_cancelled(
                    admin,
                    store,
                    &mut replay_connections,
                    &mut failing_peers,
                    &mut dropped,
                    cancel,
                ).await {
                    return;
                }
            }
            batch = subscription.recv(), if subscription_open => {
                let Some(batch) = batch else {
                    tracing::warn!(target: "gents::agent::p2p_reconcile::engine", "pairing reconciler document-change subscription closed; continuing with periodic sweeps");
                    subscription_open = false;
                    continue;
                };
                if batch.resync_required {
                    tracing::warn!(target: "gents::agent::p2p_reconcile::engine",
                        updates = batch.updates,
                        "pairing reconciler document-change capacity exceeded; performing authoritative sweep"
                    );
                }
                if !sweep_pairings_logged_until_cancelled(
                    admin,
                    store,
                    &mut replay_connections,
                    &mut failing_peers,
                    &mut dropped,
                    cancel,
                ).await {
                    return;
                }
            }
        }
    }
}

/// Record one transport connection change; true when a peer connected. A
/// disconnect is kept until a sweep consumes it, so a drop and reconnect
/// between two sweeps still replays.
fn record_peer_connection(message: &events::Message, dropped: &mut BTreeSet<String>) -> bool {
    let Some(change) = message.as_peer_connection() else {
        return false;
    };
    if !change.connected {
        dropped.insert(change.peer_id.clone());
    }
    change.connected
}

pub(super) async fn sweep_pairings_logged_until_cancelled(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
    failing_peers: &mut BTreeSet<String>,
    dropped: &mut BTreeSet<String>,
    cancel: &CancellationToken,
) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = sweep_pairings_logged(admin, store, replay_connections, failing_peers, dropped) => true,
    }
}

async fn sweep_pairings_logged(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
    failing_peers: &mut BTreeSet<String>,
    dropped: &mut BTreeSet<String>,
) {
    if let Err(error) =
        sweep_pairings_after_drops(admin, store, replay_connections, failing_peers, dropped).await
    {
        tracing::warn!(target: "gents::agent::p2p_reconcile::engine", error = %error, "pairing reconciler sweep failed; retrying on next tick");
    }
}

#[cfg(test)]
pub(super) async fn sweep_pairings(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
    failing_peers: &mut BTreeSet<String>,
) -> Result<()> {
    sweep_pairings_after_drops(
        admin,
        store,
        replay_connections,
        failing_peers,
        &mut BTreeSet::new(),
    )
    .await
}

/// Sweep every pairing; a peer whose transport `dropped` since the last sweep
/// replays even when it is connected again.
pub(super) async fn sweep_pairings_after_drops(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
    failing_peers: &mut BTreeSet<String>,
    dropped: &mut BTreeSet<String>,
) -> Result<()> {
    store.begin_sweep().await?;
    let peer_ids = store.list_peer_ids().await?;
    let mut applied = store.load_applied_many(&peer_ids).await?;
    let preparation_inputs = peer_ids
        .into_iter()
        .map(|peer_id| {
            let was_active = replay_connections.get(&peer_id).copied();
            (peer_id, was_active)
        })
        .collect::<Vec<_>>();
    let dropped_peers = &*dropped;
    let mut preparations =
        stream::iter(preparation_inputs.into_iter().map(|(peer_id, was_active)| {
            let applied = applied.remove(&peer_id).unwrap_or_default();
            async move {
                let force_replay_when_active = was_active.is_none_or(|active| !active);
                prepare_pairing_peer(
                    admin,
                    store,
                    peer_id,
                    force_replay_when_active,
                    dropped_peers,
                    Some(applied),
                )
                .await
            }
        }))
        .buffer_unordered(MAX_CONCURRENT_PEER_PREPARATIONS);

    while let Some(prepared) = preparations.next().await {
        let peer_id = prepared.peer_id.clone();
        let active_before = prepared.active_before;
        let (tick_succeeded, active_after) = match reconcile_prepared_peer(admin, store, prepared)
            .await
        {
            Ok(outcome) => {
                if outcome.desired_read_failed {
                    let was_active = replay_connections.get(&peer_id).copied().unwrap_or(false);
                    replay_connections.insert(peer_id.clone(), was_active && active_before);
                    continue;
                }
                if !outcome.ops_applied.is_empty() {
                    tracing::info!(target: "gents::agent::p2p_reconcile::engine",
                        peer_id = %outcome.peer_id,
                        ops = ?outcome.ops_applied,
                        "pairing reconcile applied operations"
                    );
                }
                if !outcome.replayed_replicators.is_empty() {
                    tracing::info!(target: "gents::agent::p2p_reconcile::engine",
                        peer_id = %outcome.peer_id,
                        replicators = ?outcome.replayed_replicators,
                        "replayed agent-target replicators after peer reconnect"
                    );
                }
                (true, outcome.peer_active)
            }
            Err(error) => {
                if failing_peers.insert(peer_id.clone()) {
                    tracing::warn!(target: "gents::agent::p2p_reconcile::engine",
                        peer_id = %peer_id,
                        error = ?error,
                        "pairing reconcile tick failing (will retry each sweep; further failures logged at debug)"
                    );
                } else {
                    tracing::debug!(target: "gents::agent::p2p_reconcile::engine",
                        peer_id = %peer_id,
                        error = ?error,
                        "pairing reconcile tick still failing"
                    );
                }
                (false, false)
            }
        };
        if tick_succeeded && failing_peers.remove(&peer_id) {
            tracing::info!(target: "gents::agent::p2p_reconcile::engine", peer_id = %peer_id, "pairing reconcile recovered for peer");
        }
        replay_connections.insert(peer_id.clone(), tick_succeeded && active_after);
    }
    drop(preparations);
    dropped.clear();
    Ok(())
}
