//! Runtime pairing reconcile engine.

mod remote_topology;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use defra_node::{EmbeddedNode, EventName};
use futures::{stream, StreamExt};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::enrollment_reconcile::EnrollmentAuthorityHandle;
use super::graphql_helpers::{ensure_no_errors, first_row, graphql_string_list_literal, rows};
#[cfg(test)]
use super::templates::Delivery;
use super::templates::{
    admit_app_collections, decode_pairing_filters, equality_filter, resolve_template, DidSource,
    PairingFilters, Scope, APP_COLLECTIONS_TEMPLATE,
};
use super::{
    compute_owned_pairing_diff, owned_pairing_live_matches, DiffOp, EmbeddedRemoteP2pAdmin,
    PairingApplied, PairingDesired, RemoteP2pAdmin,
};

#[cfg(test)]
use remote_topology::canonical_replicator_address;
pub use remote_topology::teardown_owned_replicators_at_endpoint;
use remote_topology::{apply_op, read_actual, replay_replicator_after_reconnect};

pub const PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_CONCURRENT_PEER_PREPARATIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentEndpointEntry {
    /// Durable materialization key; may be directional and is not a transport peer id.
    pub desired_id: String,
    pub peer_id: String,
    pub agent_did: String,
    pub address: String,
    pub request_digest: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentRouteGeneration {
    pub member_did: String,
    pub member_peer: String,
    pub member_ticket: String,
    pub request_digest: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
}

impl From<&super::enrollment_reconcile::EnrollmentAuthorizationFence>
    for EnrollmentRouteGeneration
{
    fn from(fence: &super::enrollment_reconcile::EnrollmentAuthorizationFence) -> Self {
        Self {
            member_did: fence.member_did.clone(),
            member_peer: fence.member_peer.clone(),
            member_ticket: fence.member_ticket.clone(),
            request_digest: fence.request_digest.clone(),
            authorization_sequence: fence.authorization_sequence,
            authorization_expires_at: fence.authorization_expires_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedPairingDesired {
    pub state: Option<PairingDesired>,
    pub enrollment_generation: Option<EnrollmentRouteGeneration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTickOutcome {
    pub peer_id: String,
    pub ops_applied: Vec<DiffOp>,
    pub replayed_replicators: Vec<String>,
    pub desired_read_failed: bool,
    pub peer_active: bool,
    /// True only when a post-reconcile live read matches the owned desired
    /// address, collection set, and effective filter.
    pub live_route_matches: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedPairingApplied {
    pub state: PairingApplied,
    pub duplicate_doc_ids: Vec<String>,
}

#[async_trait]
pub trait PairingStateStore: Send + Sync {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>>;

    async fn load_desired_with_authority(&self, peer_id: &str) -> Result<LoadedPairingDesired> {
        Ok(LoadedPairingDesired {
            state: self.load_desired(peer_id).await?,
            enrollment_generation: None,
        })
    }

    async fn enrollment_generation_is_current(
        &self,
        _generation: &EnrollmentRouteGeneration,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied>;

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()>;

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>>;

    /// Called once at the start of each sweep, before the per-peer loop. Lets a
    /// store amortize per-sweep work (e.g. computing the membership-materializable
    /// set once instead of re-verifying every signature for every peer — the
    /// O(N²) the naive per-peer gate would do). Default is a no-op.
    async fn begin_sweep(&self) -> Result<()> {
        Ok(())
    }
}

pub async fn reconcile_peer_tick(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: &str,
) -> Result<PairingTickOutcome> {
    reconcile_peer_tick_with_replay(admin, store, peer_id, false).await
}

async fn reconcile_peer_tick_with_replay(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: &str,
    force_replay: bool,
) -> Result<PairingTickOutcome> {
    let prepared = prepare_pairing_peer(admin, store, peer_id.to_string(), force_replay).await;
    reconcile_prepared_peer(admin, store, prepared).await
}

enum PreparedPairingState {
    DesiredReadFailed,
    Ready {
        desired: Option<PairingDesired>,
        enrollment_generation: Option<EnrollmentRouteGeneration>,
        applied: LoadedPairingApplied,
        reconnected: bool,
        force_replay: bool,
    },
}

struct PairingPeerPreparation {
    peer_id: String,
    active_before: bool,
    state: Result<PreparedPairingState>,
}

async fn prepare_pairing_peer(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: String,
    force_replay_when_active: bool,
) -> PairingPeerPreparation {
    let loaded_desired = match store.load_desired_with_authority(&peer_id).await {
        Ok(desired) => desired,
        Err(error) => {
            tracing::warn!(
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
    let applied = match store.load_applied(&peer_id).await {
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
    if desired_state.has_wiring() && !desired_state.replicator_addresses.is_empty() {
        let endpoint_changed = !applied.state.replicator_addresses.is_empty()
            && applied.state.replicator_addresses != desired_state.replicator_addresses;
        let endpoints = desired_state
            .replicator_addresses
            .iter()
            .filter_map(|address| super::TransportEndpoint::parse(address.clone()).ok())
            .collect::<Vec<_>>();
        for endpoint in &endpoints {
            if peer_already_active(admin, endpoint.peer_id()).await {
                active_before = true;
                break;
            }
        }
        if !endpoint_changed && active_before {
            tracing::debug!(peer_id = %peer_id, "pairing peer already connected; skipping redial");
        } else {
            if !fresh_enrollment_generation_or_close(store, enrollment_generation.as_ref()).await {
                return PairingPeerPreparation {
                    peer_id,
                    active_before,
                    state: Ok(PreparedPairingState::Ready {
                        desired: None,
                        enrollment_generation,
                        applied,
                        reconnected: false,
                        force_replay: false,
                    }),
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
                tracing::info!(
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
        state: Ok(PreparedPairingState::Ready {
            desired,
            enrollment_generation,
            applied,
            reconnected,
            force_replay: force_replay_when_active && active_before,
        }),
    }
}

async fn reconcile_prepared_peer(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    prepared: PairingPeerPreparation,
) -> Result<PairingTickOutcome> {
    let active_before = prepared.active_before;
    let peer_id = prepared.peer_id;
    let PreparedPairingState::Ready {
        mut desired,
        enrollment_generation,
        applied: mut applied_record,
        reconnected,
        force_replay,
    } = prepared.state?
    else {
        return Ok(PairingTickOutcome {
            peer_id,
            ops_applied: Vec::new(),
            replayed_replicators: Vec::new(),
            desired_read_failed: true,
            peer_active: false,
            live_route_matches: false,
        });
    };
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

    // #664 residual convergence: a subagent peer can be partitioned longer
    // than the persisted per-request redrive cap. DefraDB's idempotent
    // add_replicator path deliberately skips initial replay for an existing
    // identity, so after a real reconnect force-reinstall every otherwise-
    // converged subagent replicator. This replays current owner-authored DAG
    // heads (including arbitrarily old terminal requests) and does not create
    // any new same-value request history. If the desired identity itself
    // changed, the ordinary diff already contains teardown+install and is the
    // replay; avoid doing it twice.
    if (reconnected || force_replay)
        && desired_state.uses_subagent_template()
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
            if diff_reinstalls_address {
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
            tracing::warn!(error = %error, "enrollment authority recheck failed; closing pairing route");
            false
        }
    }
}

/// Read the live transport without applying mutations and compare it with the
/// exact desired/persisted ownership tuple. Attestation writers use this seam
/// so the pairing reconciler remains the only route writer.
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
            tracing::debug!(
                peer_id,
                error = %error,
                "active-peer read failed; assuming peer is not connected"
            );
            return false;
        }
    };
    peers.iter().any(|entry| {
        parse_public_peer_addr(entry)
            .map(|(parsed, _)| parsed.as_str() == peer_id)
            .unwrap_or_else(|_| entry.contains(peer_id))
    })
}

pub async fn run_pairing_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    enrollment: EnrollmentAuthorityHandle,
    cancel: CancellationToken,
) -> Result<()> {
    if node.p2p_arc().is_none() {
        tracing::debug!("pairing reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    }

    let admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let store =
        GraphqlPairingStateStore::with_enrollment_authority(node.clone(), identity, enrollment);
    let subscription = node.subscribe(&[EventName::Update]);

    run_pairing_reconciler_loop(&admin, &store, subscription, &cancel).await;
    Ok(())
}

async fn run_pairing_reconciler_loop(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    mut subscription: events::Subscription,
    cancel: &CancellationToken,
) {
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut replay_connections = BTreeMap::new();
    let mut failing_peers = BTreeSet::<String>::new();

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
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("pairing reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "pairing reconciler update subscription dropped messages");
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

async fn sweep_pairings_logged_until_cancelled(
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
        tracing::warn!(error = %error, "pairing reconciler sweep failed; retrying on next tick");
    }
}

async fn sweep_pairings(
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
                    tracing::info!(
                        peer_id = %outcome.peer_id,
                        ops = ?outcome.ops_applied,
                        "pairing reconcile applied operations"
                    );
                }
                if !outcome.replayed_replicators.is_empty() {
                    tracing::info!(
                        peer_id = %outcome.peer_id,
                        replicators = ?outcome.replayed_replicators,
                        "replayed subagent replicators after peer reconnect"
                    );
                }
                (true, outcome.peer_active)
            }
            Err(error) => {
                if failing_peers.insert(peer_id.clone()) {
                    tracing::warn!(
                        peer_id = %peer_id,
                        error = ?error,
                        "pairing reconcile tick failing (will retry each sweep; further failures logged at debug)"
                    );
                } else {
                    tracing::debug!(
                        peer_id = %peer_id,
                        error = ?error,
                        "pairing reconcile tick still failing"
                    );
                }
                (false, false)
            }
        };
        if tick_succeeded && failing_peers.remove(&peer_id) {
            tracing::info!(peer_id = %peer_id, "pairing reconcile recovered for peer");
        }
        replay_connections.insert(peer_id.clone(), tick_succeeded && active_after);
    }
    Ok(())
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

#[derive(Clone)]
pub struct GraphqlPairingStateStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    enrollment: Option<EnrollmentAuthorityHandle>,
    exact_enrollment: Option<EnrollmentEndpointEntry>,
}

impl GraphqlPairingStateStore {
    /// Construct a store for explicit `PeerPairingDesired` ownership only.
    ///
    /// Runtime data-plane materialization must instead use
    /// [`Self::with_enrollment_authority`] so it cannot bypass the durable
    /// enrollment projection.
    pub fn for_explicit_desired(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self {
            node,
            identity,
            enrollment: None,
            exact_enrollment: None,
        }
    }

    pub fn with_enrollment_authority(
        node: Arc<EmbeddedNode>,
        identity: Arc<dyn AgentIdentity>,
        enrollment: EnrollmentAuthorityHandle,
    ) -> Self {
        Self {
            node,
            identity,
            enrollment: Some(enrollment),
            exact_enrollment: None,
        }
    }

    /// Construct a materialization store fenced by one already-verified exact
    /// enrollment generation. The desired document is evidence only.
    pub fn for_enrollment_materialization(
        node: Arc<EmbeddedNode>,
        identity: Arc<dyn AgentIdentity>,
        entry: EnrollmentEndpointEntry,
    ) -> Self {
        Self {
            node,
            identity,
            enrollment: None,
            exact_enrollment: Some(entry),
        }
    }

    async fn data_plane_materialized_entry(
        &self,
        peer_id: &str,
    ) -> Result<Option<(MaterializedDataPlaneEntry, EnrollmentRouteGeneration)>> {
        let now = Utc::now();
        let (entry, generation) = if let Some(enrollment) = self.enrollment.as_ref() {
            let fence = match enrollment.fresh_peer_authorization(peer_id).await {
                Ok(Some(fence)) => fence,
                Ok(None) => return Ok(None),
                Err(error) => {
                    tracing::warn!(
                        peer_id,
                        error = %error,
                        "enrollment authority projection unavailable; closing materialized route"
                    );
                    return Ok(None);
                }
            };
            let generation = EnrollmentRouteGeneration::from(&fence);
            let entry = EnrollmentEndpointEntry {
                desired_id: fence.member_peer.clone(),
                peer_id: fence.member_peer,
                agent_did: fence.member_did,
                address: fence.member_ticket,
                request_digest: fence.request_digest,
                authorization_sequence: fence.authorization_sequence,
                authorization_expires_at: fence.authorization_expires_at,
            };
            (entry, generation)
        } else if let Some(exact) = self.exact_enrollment.as_ref() {
            let generation = EnrollmentRouteGeneration {
                member_did: exact.agent_did.clone(),
                member_peer: exact.peer_id.clone(),
                member_ticket: exact.address.clone(),
                request_digest: exact.request_digest.clone(),
                authorization_sequence: exact.authorization_sequence,
                authorization_expires_at: exact.authorization_expires_at.clone(),
            };
            (exact.clone(), generation)
        } else {
            return Ok(None);
        };
        if !enrollment_entry_is_fresh_at(&entry, now) {
            return Ok(None);
        }
        Ok(
            materialized_enrollment_entry(&[entry], peer_id, self.identity.did())
                .map(|entry| (entry, generation)),
        )
    }

    async fn load_applied_rows(&self, peer_id: &str) -> Result<Vec<AppliedStateRow>> {
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    _docID
                    collections
                    replicator_addresses
                    replicator_filter
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query PeerPairingApplied")?;
        let mut rows = rows::<AppliedStateRow>(&response, "PeerPairingApplied")?;
        rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
        Ok(rows)
    }
}

fn enrollment_entry_is_fresh_at(entry: &EnrollmentEndpointEntry, now: DateTime<Utc>) -> bool {
    entry.authorization_sequence > 0
        && entry.authorization_sequence <= i64::MAX as u64
        && gents_protocol::enrollment::authorization_lease_is_fresh_at(
            &entry.authorization_expires_at,
            now,
        )
}

fn materialized_enrollment_entry(
    entries: &[EnrollmentEndpointEntry],
    peer_id: &str,
    self_did: &str,
) -> Option<MaterializedDataPlaneEntry> {
    entries
        .iter()
        .find(|entry| entry.desired_id == peer_id && entry.agent_did != self_did)
        .cloned()
        .map(|endpoint| MaterializedDataPlaneEntry {
            endpoint,
            source: "enrollment",
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializedDataPlaneEntry {
    endpoint: EnrollmentEndpointEntry,
    source: &'static str,
}

impl std::ops::Deref for MaterializedDataPlaneEntry {
    type Target = EnrollmentEndpointEntry;

    fn deref(&self) -> &Self::Target {
        &self.endpoint
    }
}

fn enrollment_base_row(
    mut row: PairingStateRow,
    entry: &MaterializedDataPlaneEntry,
    self_did: &str,
) -> Option<PairingStateRow> {
    if row.source.as_deref() != Some(entry.source) {
        return None;
    }
    if row.enrollment_request_digest.as_deref() != Some(&entry.request_digest)
        || row.enrollment_authorization_sequence != Some(entry.authorization_sequence as i64)
        || row.enrollment_authorization_expires_at.as_deref()
            != Some(&entry.authorization_expires_at)
    {
        return None;
    }
    // Enrollment authority owns the full base route. The document is only a
    // materialization witness; hostile row fields cannot alter its endpoint,
    // scope, or transport identity.
    row.peer_id = Some(entry.endpoint.desired_id.clone());
    row.agent_did = Some(self_did.to_string());
    row.collections = None;
    row.replicator_addresses = Some(vec![entry.endpoint.address.clone()]);
    row.template = Some(super::templates::CLIENT_TEMPLATE.to_string());
    Some(row)
}

fn local_data_plane_row(
    mut row: PairingStateRow,
    entry: &MaterializedDataPlaneEntry,
    self_did: &str,
) -> Option<PairingStateRow> {
    let source = row.source.as_deref()?.trim();
    if source.is_empty() || source == entry.source {
        return None;
    }
    // A local data-plane document may choose only the non-protocol collection
    // overlay. Current enrollment remains the transport and identity gate.
    row.peer_id = Some(entry.endpoint.peer_id.clone());
    row.agent_did = Some(self_did.to_string());
    row.replicator_addresses = Some(vec![entry.endpoint.address.clone()]);
    Some(row)
}

#[async_trait]
impl PairingStateStore for GraphqlPairingStateStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(self.load_desired_with_authority(peer_id).await?.state)
    }

    async fn load_desired_with_authority(&self, peer_id: &str) -> Result<LoadedPairingDesired> {
        let raw_peer_id = peer_id.to_string();
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    peer_id
                    agent_did
                    replicator_addresses
                    template
                    source
                    enrollment_request_digest
                    enrollment_authorization_sequence
                    enrollment_authorization_expires_at
                }}
                DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    peer_id
                    agent_did
                    collections
                    replicator_addresses
                    template
                    source
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query pairing desired state")?;
        let materialized_entry = self
            .data_plane_materialized_entry(&raw_peer_id)
            .await
            .with_context(|| format!("checking enrollment authority for {raw_peer_id}"))?;
        let Some((entry, generation)) = materialized_entry else {
            return Ok(LoadedPairingDesired::default());
        };
        let base = first_row::<PairingStateRow>(&response, "PeerPairingDesired")?
            .and_then(|row| enrollment_base_row(row, &entry, self.identity.did()))
            .map(|row| {
                data_plane_desired_from_pairing_row(row, &entry.endpoint, self.identity.did())
            })
            .transpose()?
            .flatten();
        let data_plane = first_row::<PairingStateRow>(&response, "DataPlanePairingDesired")?
            .and_then(|row| local_data_plane_row(row, &entry, self.identity.did()))
            .map(|row| {
                data_plane_desired_from_pairing_row(row, &entry.endpoint, self.identity.did())
            })
            .transpose()?
            .flatten();
        Ok(LoadedPairingDesired {
            state: merge_layered_desired(
                self.identity.did(),
                &entry.endpoint.agent_did,
                base,
                data_plane,
            ),
            enrollment_generation: Some(generation),
        })
    }

    async fn enrollment_generation_is_current(
        &self,
        generation: &EnrollmentRouteGeneration,
    ) -> Result<bool> {
        if let Some(enrollment) = self.enrollment.as_ref() {
            return Ok(enrollment
                .fresh_authorization(&generation.member_did, &generation.member_peer)
                .await?
                .as_ref()
                .map(EnrollmentRouteGeneration::from)
                .as_ref()
                == Some(generation));
        }
        Ok(self.exact_enrollment.as_ref().is_some_and(|entry| {
            enrollment_entry_is_fresh_at(entry, Utc::now())
                && entry.agent_did == generation.member_did
                && entry.peer_id == generation.member_peer
                && entry.address == generation.member_ticket
                && entry.request_digest == generation.request_digest
                && entry.authorization_sequence == generation.authorization_sequence
                && entry.authorization_expires_at == generation.authorization_expires_at
        }))
    }

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied> {
        let mut rows = self.load_applied_rows(peer_id).await?.into_iter();
        let Some(canonical) = rows.next() else {
            return Ok(LoadedPairingApplied::default());
        };
        Ok(LoadedPairingApplied {
            state: canonical.into_applied(),
            duplicate_doc_ids: rows.map(|row| row.doc_id).collect(),
        })
    }

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        if applied.state.is_empty() {
            let peer_id = escape_graphql_string(peer_id);
            let mutation = format!(
                r#"mutation {{
                    delete_PeerPairingApplied(
                        filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}
                    ) {{ _docID }}
                }}"#
            );
            return crate::graphql::graphql_mutation_with_transaction_retry(
                &self.node,
                &mutation,
                "delete PeerPairingApplied",
            )
            .await
            .map(|_| ());
        }

        let peer_id = escape_graphql_string(peer_id);
        let collections = graphql_nullable_string_array(&applied.state.collections);
        let replicator_addresses =
            graphql_nullable_string_array(&applied.state.replicator_addresses);
        let replicator_filter = graphql_nullable_filter_literal(&applied.state.replicator_filter);
        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let save = format!(
            r#"upsert_PeerPairingApplied(
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                    add: {{
                        peer_id: "{peer_id}",
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        replicator_filter: {replicator_filter},
                        created_at: "{now}",
                        updated_at: "{now}"
                    }},
                    update: {{
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        replicator_filter: {replicator_filter},
                        updated_at: "{now}"
                    }}
                ) {{ _docID }}"#,
        );
        let delete_duplicates = if applied.duplicate_doc_ids.is_empty() {
            String::new()
        } else {
            let ids =
                graphql_string_list_literal(applied.duplicate_doc_ids.iter().map(String::as_str));
            format!(
                r#"delete_PeerPairingApplied(
                    filter: {{ _docID: {{ _in: {ids} }} }}
                ) {{ _docID }}"#
            )
        };
        let mutation = format!("mutation {{ {delete_duplicates} {save} }}");
        crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "save PeerPairingApplied",
        )
        .await
        .map(|_| ())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            PeerPairingDesired { peer_id }
            DataPlanePairingDesired { peer_id }
            PeerPairingApplied { peer_id }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query pairing peer ids")?;
        let mut ids = BTreeSet::new();
        for row in rows::<PeerIdRow>(&response, "PeerPairingDesired")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        for row in rows::<PeerIdRow>(&response, "DataPlanePairingDesired")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        for row in rows::<PeerIdRow>(&response, "PeerPairingApplied")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        Ok(ids)
    }
}

#[derive(Default, Deserialize)]
struct PairingStateRow {
    #[serde(default)]
    peer_id: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    collections: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    enrollment_request_digest: Option<String>,
    #[serde(default)]
    enrollment_authorization_sequence: Option<i64>,
    #[serde(default)]
    enrollment_authorization_expires_at: Option<String>,
}

#[derive(Deserialize)]
struct AppliedStateRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    collections: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
    #[serde(default)]
    replicator_filter: Option<String>,
}

impl AppliedStateRow {
    fn into_applied(self) -> PairingApplied {
        PairingApplied {
            collections: self.collections.unwrap_or_default().into_iter().collect(),
            replicator_addresses: self
                .replicator_addresses
                .unwrap_or_default()
                .into_iter()
                .collect(),
            replicator_filter: decode_replicator_filter(self.replicator_filter.as_deref()),
        }
    }
}

#[derive(Deserialize)]
struct PeerIdRow {
    peer_id: String,
}

#[cfg(test)]
fn desired_from_pairing_row(
    row: PairingStateRow,
    local_did: &str,
) -> Result<Option<PairingDesired>> {
    let replicator_addresses = row
        .replicator_addresses
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();

    let template_id = row
        .template
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .context("pairing row is missing its scope template")?;
    let template = resolve_template(template_id)
        .with_context(|| format!("unknown pairing scope template {template_id:?}"))?;

    if template.id == APP_COLLECTIONS_TEMPLATE {
        tracing::warn!(
            "PeerPairingDesired names the app-collections template, which is \
             data-plane-only and supplies no collections here; skipping (no wiring)"
        );
        return Ok(None);
    }

    let peer_did = row.agent_did.as_deref().map(str::trim).unwrap_or_default();
    if peer_did.is_empty() && scope_requires_peer_did(&template.scope) {
        anyhow::bail!(
            "pairing row for peer-DID-dependent template {template_id:?} has a blank \
             agent_did; refusing to install an unscoped replicator (skipping peer)"
        );
    }
    let (requester_did, owner_agent_did, direction) = if template.scope == Scope::ClientRoute {
        let route_id = row
            .peer_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("client pairing row is missing its durable route key")?;
        let direction = super::policy::client_route_direction(route_id)?;
        (local_did, peer_did, direction)
    } else {
        (
            peer_did,
            local_did,
            super::policy::PairingDirection::RuntimeToClient,
        )
    };
    let replicator_collections = if template.scope == Scope::ClientRoute {
        super::policy::client_route_collections(direction)
    } else {
        template.collections
    }
    .iter()
    .map(|&collection| collection.to_string())
    .collect::<BTreeSet<_>>();
    let replicator_filter = super::policy::resolve_template_filters(
        template,
        direction,
        requester_did,
        owner_agent_did,
    );

    let subscription_collections = match template.delivery {
        Delivery::Push => BTreeSet::new(),
        Delivery::Replicate => replicator_collections.clone(),
    };

    Ok(Some(PairingDesired {
        collections: subscription_collections,
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    }))
}

fn data_plane_desired_from_pairing_row(
    mut row: PairingStateRow,
    signed_endpoint: &EnrollmentEndpointEntry,
    self_did: &str,
) -> Result<Option<PairingDesired>> {
    let row_addresses = row
        .replicator_addresses
        .as_ref()
        .map(|values| {
            values
                .iter()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if !row_addresses.is_empty()
        && (row_addresses.len() != 1 || !row_addresses.contains(signed_endpoint.address.as_str()))
    {
        tracing::warn!(
            peer_id = %signed_endpoint.peer_id,
            signed_address = %signed_endpoint.address,
            row_addresses = ?row_addresses,
            "DataPlanePairingDesired address did not match signed PeerEndpoint; using signed endpoint address"
        );
    }

    if let Some(row_did) = row
        .agent_did
        .as_deref()
        .map(str::trim)
        .filter(|did| !did.is_empty())
    {
        if row_did != self_did {
            anyhow::bail!(
                "DataPlanePairingDesired for peer {} scopes agent_did {} but this node is {}; \
                 refusing to install a data-plane replicator for a foreign DID",
                signed_endpoint.peer_id,
                row_did,
                self_did
            );
        }
    }

    let template_id = row
        .template
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .context("data-plane pairing row is missing its scope template")?;
    let template = resolve_template(template_id)
        .with_context(|| format!("unknown data-plane pairing scope template {template_id:?}"))?;
    let client_route_direction = if template.scope == Scope::ClientRoute {
        if row.source.as_deref() == Some("enrollment") {
            super::policy::PairingDirection::RuntimeToClient
        } else {
            let route_id = row
                .peer_id
                .as_deref()
                .map(str::trim)
                .filter(|route_id| !route_id.is_empty())
                .context("client data-plane pairing row is missing its durable route key")?;
            super::policy::client_route_direction(route_id)?
        }
    } else {
        super::policy::PairingDirection::RuntimeToClient
    };
    let peer_did = signed_endpoint.agent_did.trim();
    if data_plane_scope_requires_signed_peer_did(&template.scope) && peer_did.is_empty() {
        anyhow::bail!(
            "DataPlanePairingDesired for peer {} uses template {template_id:?} but the signed \
             PeerEndpoint has a blank agent_did",
            signed_endpoint.peer_id
        );
    }

    row.replicator_addresses = Some(vec![signed_endpoint.address.clone()]);
    let replicator_addresses = row
        .replicator_addresses
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();

    let (replicator_collections, subscription_collections): (BTreeSet<String>, BTreeSet<String>) =
        if template.id == APP_COLLECTIONS_TEMPLATE {
            let requested = row
                .collections
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect::<BTreeSet<_>>();
            let Some(row_cols) = admit_app_collections(requested.clone()) else {
                tracing::warn!(
                    peer_id = %signed_endpoint.peer_id,
                    collections = ?requested,
                    "app-collections DataPlanePairingDesired is empty or overlaps the protocol \
                     catalog; skipping this data-plane layer (control pairing unaffected)"
                );
                return Ok(None);
            };
            (row_cols.clone(), row_cols)
        } else {
            let collections = if template.scope == Scope::ClientRoute {
                super::policy::client_route_collections(client_route_direction)
            } else {
                template.collections
            };
            let cols = collections
                .iter()
                .map(|&c| c.to_string())
                .collect::<BTreeSet<_>>();
            (cols, BTreeSet::new())
        };

    let filter_collections = replicator_collections
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let replicator_filter = data_plane_scope_filter(
        &template.scope,
        &filter_collections,
        peer_did,
        self_did,
        client_route_direction,
    );

    Ok(Some(PairingDesired {
        collections: subscription_collections,
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    }))
}

#[cfg(test)]
fn scope_requires_peer_did(scope: &Scope) -> bool {
    match scope {
        Scope::PeerDid { .. } => true,
        Scope::Unscoped => false,
        Scope::PerCollection(rules) => rules
            .iter()
            .any(|rule| matches!(rule.source, DidSource::PeerDid)),
        Scope::ClientRoute => true,
    }
}

fn data_plane_scope_requires_signed_peer_did(scope: &Scope) -> bool {
    match scope {
        Scope::PeerDid { .. } | Scope::Unscoped => false,
        Scope::PerCollection(rules) => rules
            .iter()
            .any(|rule| matches!(rule.source, DidSource::PeerDid)),
        Scope::ClientRoute => true,
    }
}

fn data_plane_scope_filter(
    scope: &Scope,
    collections: &[&str],
    signed_peer_did: &str,
    local_did: &str,
    client_route_direction: super::policy::PairingDirection,
) -> PairingFilters {
    match scope {
        Scope::PeerDid { field } => collections
            .iter()
            .map(|&col| (col.to_string(), equality_filter(*field, local_did)))
            .collect(),
        Scope::Unscoped => BTreeMap::new(),
        Scope::PerCollection(rules) => rules
            .iter()
            .map(|rule| {
                let value = match rule.source {
                    DidSource::LocalDid => local_did,
                    DidSource::PeerDid => signed_peer_did,
                    DidSource::HomeDid => local_did,
                };
                (
                    rule.collection.to_string(),
                    equality_filter(rule.field, value),
                )
            })
            .collect(),
        Scope::ClientRoute => {
            let (requester_did, owner_agent_did) = match client_route_direction {
                super::policy::PairingDirection::ClientToRuntime => (local_did, signed_peer_did),
                super::policy::PairingDirection::RuntimeToClient => (signed_peer_did, local_did),
            };
            super::policy::resolve_template_filters(
                resolve_template(super::templates::CLIENT_TEMPLATE).expect("client template"),
                client_route_direction,
                requester_did,
                owner_agent_did,
            )
        }
    }
}

pub fn merge_layered_desired(
    local_did: &str,
    peer_did: &str,
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    let base = if peer_did == local_did { None } else { base };
    let data_plane = data_plane.map(|mut desired| {
        if !desired.template_ids.contains(APP_COLLECTIONS_TEMPLATE) {
            desired.collections.clear();
        }
        desired
    });
    match (base, data_plane) {
        (None, None) => None,
        (Some(desired), None) | (None, Some(desired)) => Some(desired),
        (Some(mut left), Some(right)) => {
            left.collections.extend(right.collections);
            // The data-plane layer comes from the verified current PeerEndpoint.
            if !right.replicator_addresses.is_empty() {
                left.replicator_addresses = right.replicator_addresses;
            }
            left.replicator_collections
                .extend(right.replicator_collections);
            left.replicator_filter.extend(right.replicator_filter);
            left.template_ids.extend(right.template_ids);
            Some(left)
        }
    }
}

fn graphql_nullable_filter_literal(filter: &PairingFilters) -> String {
    if filter.is_empty() {
        return "null".to_string();
    }
    let json = serde_json::to_string(filter).unwrap_or_default();
    format!(r#""{}""#, escape_graphql_string(&json))
}

fn decode_replicator_filter(value: Option<&str>) -> PairingFilters {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return PairingFilters::default();
    };
    decode_pairing_filters(raw).unwrap_or_else(|error| {
        tracing::warn!(
            error = %error,
            "PeerPairingApplied.replicator_filter failed to decode; treating as unfiltered"
        );
        PairingFilters::default()
    })
}

fn graphql_nullable_string_array(values: &BTreeSet<String>) -> String {
    graphql_string_list_literal(values.iter().map(String::as_str))
}

#[cfg(test)]
mod tests;
