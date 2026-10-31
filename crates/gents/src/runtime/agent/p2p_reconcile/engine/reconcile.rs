use anyhow::{Context, Result};
use p2p::iroh::parse_public_peer_addr;

use super::super::templates::PairingFilters;
use super::super::{
    compute_owned_pairing_diff, owned_pairing_live_matches, DiffOp, PairingApplied, PairingDesired,
    RemoteP2pAdmin,
};
use super::remote_topology::{
    apply_op, read_actual, replay_replicator_after_reconnect, replicator_acknowledged,
};
use super::{
    EnrollmentRouteGeneration, LoadedPairingApplied, PairingStateStore, PairingTickOutcome,
};

pub async fn reconcile_peer_tick(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: &str,
) -> Result<PairingTickOutcome> {
    reconcile_peer_tick_with_replay(admin, store, peer_id, false).await
}

pub(super) async fn reconcile_peer_tick_with_replay(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: &str,
    force_replay: bool,
) -> Result<PairingTickOutcome> {
    let prepared = prepare_pairing_peer(
        admin,
        store,
        peer_id.to_string(),
        force_replay,
        &std::collections::BTreeSet::new(),
        None,
    )
    .await;
    reconcile_prepared_peer(admin, store, prepared).await
}

enum PreparedPairingState {
    DesiredReadFailed,
    Ready(Box<ReadyPairing>),
}

struct ReadyPairing {
    desired: Option<PairingDesired>,
    enrollment_generation: Option<EnrollmentRouteGeneration>,
    applied: LoadedPairingApplied,
    reconnected: bool,
    force_replay: bool,
}

pub(super) struct PairingPeerPreparation {
    pub(super) peer_id: String,
    pub(super) active_before: bool,
    state: Result<PreparedPairingState>,
}

pub(super) async fn prepare_pairing_peer(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: String,
    force_replay_when_active: bool,
    dropped: &std::collections::BTreeSet<String>,
    applied: Option<LoadedPairingApplied>,
) -> PairingPeerPreparation {
    let loaded_desired = match store.load_desired_with_authority(&peer_id).await {
        Ok(desired) => desired,
        Err(error) => {
            tracing::warn!(target: "gents::runtime::agent::p2p_reconcile::engine",
                peer_id = %peer_id,
                error = %error,
                "pairing desired state read failed; skipping reconcile preparation"
            );
            return PairingPeerPreparation {
                peer_id,
                active_before: false,
                state: Ok(PreparedPairingState::DesiredReadFailed),
            };
        }
    };
    let mut desired = loaded_desired.state;
    let enrollment_generation = loaded_desired.enrollment_generation;
    if desired.is_some()
        && !fresh_enrollment_generation_or_close(store, enrollment_generation.as_ref()).await
    {
        desired = None;
    }
    let desired_state = desired.clone().unwrap_or_default();
    let applied = match match applied {
        Some(applied) => Ok(applied),
        None => store.load_applied(&peer_id).await,
    } {
        Ok(applied) => applied,
        Err(error) => {
            return PairingPeerPreparation {
                peer_id,
                active_before: false,
                state: Err(error),
            };
        }
    };
    let mut active_before = false;
    let mut reconnected = false;
    let mut dropped_since_sweep = false;
    if desired_state.has_wiring() && !desired_state.replicator_addresses.is_empty() {
        let endpoint_changed = !applied.state.replicator_addresses.is_empty()
            && applied.state.replicator_addresses != desired_state.replicator_addresses;
        let endpoints = desired_state
            .replicator_addresses
            .iter()
            .filter_map(|address| super::super::TransportEndpoint::parse(address.clone()).ok())
            .collect::<Vec<_>>();
        dropped_since_sweep = endpoints.iter().any(|endpoint| {
            dropped
                .iter()
                .any(|entry| transport_entry_names_peer(entry, endpoint.peer_id()))
        });
        for endpoint in &endpoints {
            if peer_already_active(admin, endpoint.peer_id()).await {
                active_before = true;
                break;
            }
        }
        if !endpoint_changed && active_before {
            tracing::debug!(target: "gents::runtime::agent::p2p_reconcile::engine", peer_id = %peer_id, "pairing peer already connected; skipping redial");
        } else {
            if !fresh_enrollment_generation_or_close(store, enrollment_generation.as_ref()).await {
                return PairingPeerPreparation {
                    peer_id,
                    active_before,
                    state: Ok(PreparedPairingState::Ready(Box::new(ReadyPairing {
                        desired: None,
                        enrollment_generation,
                        applied,
                        reconnected: false,
                        force_replay: false,
                    }))),
                };
            }
            let addresses = desired_state
                .replicator_addresses
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            if let Err(error) = admin
                .connect(&addresses)
                .await
                .context("connect pairing peer")
            {
                return PairingPeerPreparation {
                    peer_id,
                    active_before,
                    state: Err(error),
                };
            }
            reconnected = true;
            if endpoint_changed {
                tracing::info!(target: "gents::runtime::agent::p2p_reconcile::engine",
                    peer_id = %peer_id,
                    previous_addresses = ?applied.state.replicator_addresses,
                    desired_addresses = ?desired_state.replicator_addresses,
                    "pairing endpoint changed; refreshed peer connection before reconcile"
                );
            }
        }
    }

    PairingPeerPreparation {
        peer_id,
        active_before,
        state: Ok(PreparedPairingState::Ready(Box::new(ReadyPairing {
            desired,
            enrollment_generation,
            applied,
            reconnected,
            force_replay: (force_replay_when_active || dropped_since_sweep) && active_before,
        }))),
    }
}

pub(super) async fn reconcile_prepared_peer(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    prepared: PairingPeerPreparation,
) -> Result<PairingTickOutcome> {
    let active_before = prepared.active_before;
    let peer_id = prepared.peer_id;
    let PreparedPairingState::Ready(ready) = prepared.state? else {
        return Ok(PairingTickOutcome {
            peer_id,
            ops_applied: Vec::new(),
            replayed_replicators: Vec::new(),
            desired_read_failed: true,
            peer_active: false,
            live_route_matches: false,
        });
    };
    let ReadyPairing {
        mut desired,
        enrollment_generation,
        applied: mut applied_record,
        reconnected,
        force_replay,
    } = *ready;
    let mut desired_state = desired.clone().unwrap_or_default();
    let mut actual = read_actual(admin, &applied_record.state.replicator_addresses).await?;

    if !applied_record.duplicate_doc_ids.is_empty() {
        persist_applied_record(store, &peer_id, &mut applied_record).await?;
    }

    if desired.is_some()
        && !fresh_enrollment_generation_or_close(store, enrollment_generation.as_ref()).await
    {
        desired = None;
        desired_state = PairingDesired::default();
    }
    let mut ops = std::collections::VecDeque::from(compute_owned_pairing_diff(
        &desired_state,
        &actual.state,
        &applied_record.state,
    ));
    let mut ops_applied = Vec::new();
    let mut replayed_replicators = Vec::new();

    if (!ops.is_empty() || reconnected || force_replay)
        && applied_record.enrollment != Default::default()
    {
        applied_record.enrollment = Default::default();
        persist_applied_record(store, &peer_id, &mut applied_record).await?;
    }

    // DefraDB's idempotent add_replicator skips initial replay for an existing
    // identity (#664), so after a reconnect force-reinstall every otherwise-
    // converged agent-target replicator whose peer has not acknowledged every
    // routed document, replaying owner-authored DAG heads. If the desired
    // identity changed, the diff already contains the teardown+install replay;
    // avoid doing it twice.
    if (reconnected || force_replay)
        && desired_state.uses_agent_target_template()
        && fresh_enrollment_generation_or_close(store, enrollment_generation.as_ref()).await
    {
        for address in desired_state
            .replicator_addresses
            .intersection(&actual.state.replicator_addresses)
        {
            let diff_reinstalls_address = ops.iter().any(|op| {
                matches!(
                    op,
                    DiffOp::InstallReplicator(candidate)
                        | DiffOp::TeardownReplicator(candidate)
                        if candidate == address
                )
            });
            if diff_reinstalls_address
                || replicator_acknowledged(admin, address, &desired_state, &actual).await
            {
                continue;
            }
            replay_replicator_after_reconnect(admin, address, &desired_state, &actual).await?;
            replayed_replicators.push(address.clone());
        }
    }

    while let Some(op) = ops.pop_front() {
        if desired.is_some()
            && !fresh_enrollment_generation_or_close(store, enrollment_generation.as_ref()).await
        {
            desired = None;
            desired_state = PairingDesired::default();
            actual = read_actual(admin, &applied_record.state.replicator_addresses)
                .await
                .context("reload live pairing state after enrollment authority closed")?;
            ops = std::collections::VecDeque::from(compute_owned_pairing_diff(
                &desired_state,
                &actual.state,
                &applied_record.state,
            ));
            continue;
        }
        apply_op(admin, &op, &desired_state, &actual).await?;
        update_applied_after_success(&mut applied_record.state, &op, &desired_state);
        persist_applied_record(store, &peer_id, &mut applied_record).await?;
        ops_applied.push(op);
    }

    let verified_actual = if ops_applied.is_empty() && replayed_replicators.is_empty() {
        actual
    } else {
        read_actual(admin, &applied_record.state.replicator_addresses)
            .await
            .context("verify live pairing state after reconcile")?
    };
    let live_route_matches = owned_pairing_live_matches(
        &desired_state,
        &verified_actual.state,
        &applied_record.state,
    );

    if desired.is_none() && !applied_record.state.is_empty() {
        applied_record.state = PairingApplied::default();
        persist_applied_record(store, &peer_id, &mut applied_record).await?;
    }
    let materialization = if desired.is_some()
        && !applied_record.state.is_empty()
        && live_route_matches
        && fresh_enrollment_generation_or_close(store, enrollment_generation.as_ref()).await
    {
        enrollment_generation
            .as_ref()
            .map(Into::into)
            .unwrap_or_default()
    } else {
        Default::default()
    };
    if applied_record.enrollment != materialization {
        applied_record.enrollment = materialization;
        persist_applied_record(store, &peer_id, &mut applied_record).await?;
    }
    Ok(PairingTickOutcome {
        peer_id,
        ops_applied,
        replayed_replicators,
        desired_read_failed: false,
        peer_active: active_before || reconnected,
        live_route_matches,
    })
}

async fn fresh_enrollment_generation_or_close(
    store: &dyn PairingStateStore,
    generation: Option<&EnrollmentRouteGeneration>,
) -> bool {
    let Some(generation) = generation else {
        return true;
    };
    match store.enrollment_generation_is_current(generation).await {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(target: "gents::runtime::agent::p2p_reconcile::engine", error = %error, "enrollment authority recheck failed; closing pairing route");
            false
        }
    }
}

/// Read-only observation seam for attestation writers; the pairing reconciler
/// remains the only route writer.
pub async fn observe_owned_pairing_live_matches(
    admin: &dyn RemoteP2pAdmin,
    desired: &PairingDesired,
    applied: &PairingApplied,
) -> Result<bool> {
    let actual = read_actual(admin, &applied.replicator_addresses).await?;
    Ok(owned_pairing_live_matches(desired, &actual.state, applied))
}

async fn persist_applied_record(
    store: &dyn PairingStateStore,
    peer_id: &str,
    applied: &mut LoadedPairingApplied,
) -> Result<()> {
    store.persist_applied(peer_id, applied).await?;
    applied.duplicate_doc_ids.clear();
    Ok(())
}

async fn peer_already_active(admin: &dyn RemoteP2pAdmin, peer_id: &str) -> bool {
    let peers = match admin.active_peers().await {
        Ok(peers) => peers,
        Err(error) => {
            tracing::debug!(target: "gents::runtime::agent::p2p_reconcile::engine",
                peer_id,
                error = %error,
                "active-peer read failed; assuming peer is not connected"
            );
            return false;
        }
    };
    peers
        .iter()
        .any(|entry| transport_entry_names_peer(entry, peer_id))
}

fn transport_entry_names_peer(entry: &str, peer_id: &str) -> bool {
    parse_public_peer_addr(entry)
        .map(|(parsed, _)| parsed.as_str() == peer_id)
        .unwrap_or_else(|_| entry.contains(peer_id))
}

pub fn update_applied_after_success(
    applied: &mut PairingApplied,
    op: &DiffOp,
    desired: &PairingDesired,
) {
    match op {
        DiffOp::InstallCollection(collection) => {
            applied.collections.insert(collection.clone());
        }
        DiffOp::TeardownCollection(collection) => {
            applied.collections.remove(collection);
        }
        DiffOp::InstallReplicator(address) => {
            applied.replicator_addresses.insert(address.clone());
            applied.replicator_filter = desired.replicator_filter.clone();
        }
        DiffOp::TeardownReplicator(address) => {
            applied.replicator_addresses.remove(address);
            if applied.replicator_addresses.is_empty() {
                applied.replicator_filter = PairingFilters::default();
            }
        }
    }
}
