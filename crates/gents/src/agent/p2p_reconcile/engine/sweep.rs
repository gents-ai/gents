use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use futures::{stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::identity::NodeIdentity;

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
    // Subscribe before resolving IDs so commits racing startup remain pending.
    // Pairing intent and its applied witness are the only document state this
    // reconciler reads directly. Enrollment authority changes materialize or
    // remove PeerPairingDesired; lease expiry remains covered by the periodic
    // sweep. Transcript/config writes must not trigger a global topology scan.
    let subscription = node.subscribe_document_changes();
    let watched_collection_ids = pairing_state_collection_ids(&node)?;

    run_pairing_reconciler_loop(
        &admin,
        &store,
        subscription,
        &watched_collection_ids,
        &cancel,
    )
    .await;
    Ok(())
}

fn pairing_state_collection_ids(node: &EmbeddedNode) -> Result<BTreeSet<String>> {
    PAIRING_STATE_COLLECTIONS
        .into_iter()
        .map(|name| {
            node.get_collection(name)?
                .map(|collection| collection.collection_id)
                .with_context(|| format!("{name} schema is not installed"))
        })
        .collect()
}

pub(super) fn document_changes_wake_pairing_reconcile(
    batch: &events::DocumentChangeBatch,
    watched_collection_ids: &BTreeSet<String>,
) -> bool {
    batch.resync_required
        || batch
            .changes
            .iter()
            .any(|change| watched_collection_ids.contains(&change.collection_id))
}

pub(super) async fn run_pairing_reconciler_loop(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    mut subscription: events::DocumentChangeSubscription,
    watched_collection_ids: &BTreeSet<String>,
    cancel: &CancellationToken,
) {
    let mut interval = tokio::time::interval(super::super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut replay_connections = BTreeMap::new();
    let mut failing_peers = BTreeSet::<String>::new();
    let mut subscription_open = true;

    // A transient top-level read failure during the first sweep is no more
    // terminal than one during a recurring sweep. The interval's first tick is
    // immediately ready, so logging here preserves the prompt startup retry
    // without introducing a separate backoff path. Unlike the registry and
    // endpoint heartbeat daemons, do not consume that first tick before this
    // sweep: it is the retry fence if startup hits a transient store error.
    if !sweep_pairings_logged_until_cancelled(
        admin,
        store,
        &mut replay_connections,
        &mut failing_peers,
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
                if !document_changes_wake_pairing_reconcile(&batch, watched_collection_ids) {
                    continue;
                }
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
                    cancel,
                ).await {
                    return;
                }
            }
        }
    }
}

pub(super) async fn sweep_pairings_logged_until_cancelled(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
    failing_peers: &mut BTreeSet<String>,
    cancel: &CancellationToken,
) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = sweep_pairings_logged(admin, store, replay_connections, failing_peers) => true,
    }
}

/// Run a sweep, logging (not propagating) a transient failure. A failed sweep —
/// e.g. a momentary `list_peer_ids` read error — must not tear down the whole
/// reconciler; the next tick retries. Mirrors the discovery / heartbeat daemons,
/// which also log-and-continue rather than aborting the runtime task.
async fn sweep_pairings_logged(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
    failing_peers: &mut BTreeSet<String>,
) {
    if let Err(error) = sweep_pairings(admin, store, replay_connections, failing_peers).await {
        tracing::warn!(target: "gents::agent::p2p_reconcile::engine", error = %error, "pairing reconciler sweep failed; retrying on next tick");
    }
}

pub(super) async fn sweep_pairings(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
    failing_peers: &mut BTreeSet<String>,
) -> Result<()> {
    // Amortize the membership-materializable computation across the whole sweep
    // (avoids re-verifying every signature per peer). Non-fatal on failure: the
    // per-peer gate falls back to a live read.
    store.begin_sweep().await?;
    let preparation_inputs = store
        .list_peer_ids()
        .await?
        .into_iter()
        .map(|peer_id| {
            let was_active = replay_connections.get(&peer_id).copied();
            (peer_id, was_active)
        })
        .collect::<Vec<_>>();
    let mut preparations = stream::iter(preparation_inputs.into_iter().map(
        |(peer_id, was_active)| async move {
            let force_replay_when_active = was_active.is_none_or(|active| !active);
            prepare_pairing_peer(admin, store, peer_id, force_replay_when_active).await
        },
    ))
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
    Ok(())
}
