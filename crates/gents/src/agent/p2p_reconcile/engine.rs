//! Runtime pairing reconcile engine.

mod remote_topology;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName};
use futures::{stream, StreamExt};
use gents_protocol::bearer_token::{derive_bearer_readiness_key, BearerPairingReadyRecord};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::graphql_helpers::{ensure_no_errors, first_row, graphql_string_list_literal, rows};
use super::network::{GraphqlNetworkStore, NetworkEndpointEntry, NetworkStore};
use super::reciprocal::GraphqlReciprocalStore;
use super::templates::{
    admit_app_collections, conjunctive_string_eq, decode_pairing_filters, equality_filter,
    resolve_template, Delivery, DidSource, PairingFilters, Scope, APP_COLLECTIONS_TEMPLATE,
};
#[cfg(test)]
use super::templates::{scope_filter, FilterPredicate};
use super::{
    compute_owned_pairing_diff, owned_pairing_live_matches, DiffOp, EmbeddedRemoteP2pAdmin,
    PairingApplied, PairingDesired, RemoteP2pAdmin,
};

#[cfg(test)]
use remote_topology::canonical_replicator_address;
use remote_topology::{apply_op, read_actual, replay_replicator_after_reconnect};
pub use remote_topology::{
    teardown_owned_replicators_at_endpoint, teardown_unowned_replicators_at_endpoint,
};

pub const PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_CONCURRENT_PEER_PREPARATIONS: usize = 8;

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

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied>;

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()>;

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>>;

    async fn reconcile_bearer_readiness(
        &self,
        _peer_id: &str,
        _desired: Option<&PairingDesired>,
        _applied: &PairingApplied,
    ) -> Result<()> {
        Ok(())
    }

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
    let desired = match store.load_desired(&peer_id).await {
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
        if endpoints.is_empty() {
            // Additive compatibility for pre-Iroh and synthetic pairings.
            // A valid dialable endpoint always wins over this opaque key.
            active_before = peer_already_active(admin, &peer_id).await;
        }
        if !endpoint_changed && active_before {
            tracing::debug!(peer_id = %peer_id, "pairing peer already connected; skipping redial");
        } else {
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
        desired,
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
    let desired_state = desired.clone().unwrap_or_default();
    let actual = read_actual(admin, &applied_record.state.replicator_addresses).await?;

    if !applied_record.duplicate_doc_ids.is_empty() {
        persist_applied_record(store, &peer_id, &mut applied_record).await?;
    }

    let ops = compute_owned_pairing_diff(&desired_state, &actual.state, &applied_record.state);
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
    if (reconnected || force_replay) && desired_state.uses_subagent_template() {
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

    for op in ops {
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
    store
        .reconcile_bearer_readiness(&peer_id, desired.as_ref(), &applied_record.state)
        .await?;

    Ok(PairingTickOutcome {
        peer_id,
        ops_applied,
        replayed_replicators,
        desired_read_failed: false,
        peer_active: active_before || reconnected,
        live_route_matches,
    })
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
    cancel: CancellationToken,
) -> Result<()> {
    if node.p2p_arc().is_none() {
        tracing::debug!("pairing reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    }

    let admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let store = GraphqlPairingStateStore::new(node.clone(), identity);
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
    /// Per-sweep cache of the membership-materializable entries, refreshed by
    /// [`begin_sweep`](PairingStateStore::begin_sweep). `Some` during a sweep so
    /// the Layer-2 gate verifies every signature ONCE per sweep instead of once
    /// per peer (avoids O(N²) crypto). `None` ⇒ no cached set, fall back to a
    /// live read (also the path for the very first read or a refresh failure).
    materializable_cache: Arc<Mutex<Option<Vec<NetworkEndpointEntry>>>>,
    reciprocal_materializable_cache: Arc<Mutex<Option<Vec<NetworkEndpointEntry>>>>,
}

impl GraphqlPairingStateStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self {
            node,
            identity,
            materializable_cache: Arc::new(Mutex::new(None)),
            reciprocal_materializable_cache: Arc::new(Mutex::new(None)),
        }
    }

    async fn data_plane_materialized_entry(
        &self,
        peer_id: &str,
    ) -> Result<Option<NetworkEndpointEntry>> {
        let cached = self.materializable_cache.lock().unwrap().clone();
        let entries = match cached {
            Some(entries) => entries,
            None => {
                let network = GraphqlNetworkStore::new(self.node.clone(), self.identity.clone());
                network.load_materializable_entries().await?
            }
        };
        if let Some(entry) =
            data_plane_materialized_entry_from_sources(&entries, &[], peer_id, self.identity.did())
        {
            return Ok(Some(entry));
        }

        let cached = self.reciprocal_materializable_cache.lock().unwrap().clone();
        let reciprocal_entries = match cached {
            Some(entries) => entries,
            None => {
                let reciprocal =
                    GraphqlReciprocalStore::new(self.node.clone(), self.identity.clone());
                reciprocal.load_materializable_entries().await?
            }
        };
        Ok(data_plane_materialized_entry_from_sources(
            &[],
            &reciprocal_entries,
            peer_id,
            self.identity.did(),
        ))
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

    async fn bearer_readiness_is_current(
        &self,
        readiness_key: &str,
        expected: &BearerPairingReadyRecord,
    ) -> Result<(bool, usize)> {
        let readiness_key = escape_graphql_string(readiness_key);
        let query = format!(
            r#"{{
                BearerPairingReady(
                    filter: {{ readiness_key: {{ _eq: "{readiness_key}" }} }}
                ) {{
                    issuer_did
                    claimant_did
                    peer_id
                    address
                    template
                    acknowledged_at
                    issuer_sig
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query BearerPairingReady")?;
        let rows = rows::<BearerPairingReadyRow>(&response, "BearerPairingReady")?;
        let row_count = rows.len();
        let mut current = false;
        for row in &rows {
            let existing = match bearer_pairing_ready_record(&row) {
                Ok(Some(existing)) => existing,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "failed to decode existing bearer readiness acknowledgement"
                    );
                    continue;
                }
            };
            if existing.issuer_did != expected.issuer_did
                || existing.claimant_did != expected.claimant_did
                || existing.peer_id != expected.peer_id
                || existing.address != expected.address
                || existing.template != expected.template
            {
                continue;
            }
            match self
                .identity
                .verify(
                    &existing.issuer_did,
                    &existing.signing_payload(),
                    &existing.sig,
                )
                .await
            {
                Ok(true) => current = true,
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    error = %error,
                    claimant_did = %existing.claimant_did,
                    "failed to verify existing bearer readiness acknowledgement"
                ),
            }
        }
        Ok((current, row_count))
    }

    async fn upsert_bearer_readiness(
        &self,
        peer_id: &str,
        claimant_did: &str,
        address: &str,
        template: &str,
    ) -> Result<()> {
        let readiness_key = derive_bearer_readiness_key(self.identity.did(), claimant_did);
        let mut record = BearerPairingReadyRecord {
            issuer_did: self.identity.did().to_string(),
            claimant_did: claimant_did.to_string(),
            peer_id: peer_id.to_string(),
            address: address.to_string(),
            template: template.to_string(),
            acknowledged_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            sig: Vec::new(),
        };
        let (current, row_count) = self
            .bearer_readiness_is_current(&readiness_key, &record)
            .await?;
        if current && row_count == 1 {
            return Ok(());
        }
        record.sig = self
            .identity
            .sign(&record.signing_payload())
            .await
            .context("signing bearer pairing readiness acknowledgement")?;
        let mutation = bearer_pairing_ready_upsert_mutation(&readiness_key, &record);
        crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "upsert BearerPairingReady",
        )
        .await
        .map(|_| ())
    }

    async fn delete_bearer_readiness_for_peer(&self, peer_id: &str) -> Result<()> {
        let peer_id = escape_graphql_string(peer_id);
        let issuer_did = escape_graphql_string(self.identity.did());
        let query = format!(
            r#"{{
                BearerPairingReady(
                    filter: {{
                        peer_id: {{ _eq: "{peer_id}" }},
                        issuer_did: {{ _eq: "{issuer_did}" }}
                    }},
                    limit: 1
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query BearerPairingReady for deletion")?;
        if rows::<DocIdRow>(&response, "BearerPairingReady")?.is_empty() {
            return Ok(());
        }

        let mutation = format!(
            r#"mutation {{
                delete_BearerPairingReady(
                    filter: {{
                        peer_id: {{ _eq: "{peer_id}" }},
                        issuer_did: {{ _eq: "{issuer_did}" }}
                    }}
                ) {{ _docID }}
            }}"#
        );
        crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "delete BearerPairingReady",
        )
        .await
        .map(|_| ())
    }
}

fn data_plane_materialized_entry_from_sources(
    network_entries: &[NetworkEndpointEntry],
    reciprocal_entries: &[NetworkEndpointEntry],
    peer_id: &str,
    self_did: &str,
) -> Option<NetworkEndpointEntry> {
    super::network::materializable_entry_for_peer(network_entries, peer_id, self_did)
        .or_else(|| {
            super::network::materializable_entry_for_peer(reciprocal_entries, peer_id, self_did)
        })
        .cloned()
}

#[async_trait]
impl PairingStateStore for GraphqlPairingStateStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        let raw_peer_id = peer_id.to_string();
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    peer_id
                    agent_did
                    replicator_addresses
                    template
                }}
                DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    peer_id
                    agent_did
                    collections
                    replicator_addresses
                    template
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query pairing desired state")?;
        let base_row = first_row::<PairingStateRow>(&response, "PeerPairingDesired")?;
        let base_peer_did = base_row
            .as_ref()
            .and_then(|row| row.agent_did.as_deref())
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let base = base_row
            .map(|row| desired_from_pairing_row(row, self.identity.did()))
            .transpose()?
            .flatten();
        let materialized_entry = self
            .data_plane_materialized_entry(&raw_peer_id)
            .await
            .with_context(|| format!("checking network membership gate for {raw_peer_id}"))?;
        let data_plane = match (
            materialized_entry,
            first_row::<PairingStateRow>(&response, "DataPlanePairingDesired")?,
        ) {
            (Some(entry), Some(row)) => {
                data_plane_desired_from_pairing_row(row, &entry, self.identity.did())?
            }
            _ => None,
        };
        Ok(merge_layered_desired(
            self.identity.did(),
            &base_peer_did,
            base,
            data_plane,
        ))
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

    async fn reconcile_bearer_readiness(
        &self,
        peer_id: &str,
        desired: Option<&PairingDesired>,
        applied: &PairingApplied,
    ) -> Result<()> {
        let Some((claimant_did, address, template)) =
            earned_bearer_readiness(desired, applied, self.identity.did())
        else {
            return self.delete_bearer_readiness_for_peer(peer_id).await;
        };
        self.upsert_bearer_readiness(peer_id, &claimant_did, &address, &template)
            .await
    }

    async fn begin_sweep(&self) -> Result<()> {
        // Compute the membership-materializable set ONCE for this sweep so the
        // per-peer Layer-2 gate (`data_plane_peer_is_materializable`) reuses it
        // instead of re-verifying every membership/endpoint signature for every
        // peer (the O(N²) the review flagged). A refresh failure is non-fatal:
        // clear the cache so the per-peer path falls back to a live read.
        let network = GraphqlNetworkStore::new(self.node.clone(), self.identity.clone());
        let refreshed = match network.load_materializable_entries().await {
            Ok(entries) => Some(entries),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "materializable-set refresh failed; per-peer gate will read live this sweep"
                );
                None
            }
        };
        *self.materializable_cache.lock().unwrap() = refreshed;

        let reciprocal = GraphqlReciprocalStore::new(self.node.clone(), self.identity.clone());
        let reciprocal_refreshed = match reciprocal.load_materializable_entries().await {
            Ok(entries) => Some(entries),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "reciprocal materializable-set refresh failed; per-peer gate will read live this sweep"
                );
                None
            }
        };
        *self.reciprocal_materializable_cache.lock().unwrap() = reciprocal_refreshed;
        Ok(())
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

pub const DEFAULT_PAIRING_TEMPLATE: &str = "conversation";

#[derive(Deserialize)]
struct PeerIdRow {
    peer_id: String,
}

#[derive(Deserialize)]
struct BearerPairingReadyRow {
    issuer_did: Option<String>,
    claimant_did: Option<String>,
    peer_id: Option<String>,
    address: Option<String>,
    template: Option<String>,
    acknowledged_at: Option<String>,
    issuer_sig: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DocIdRow {
    #[serde(rename = "_docID")]
    _doc_id: String,
}

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
        .unwrap_or(DEFAULT_PAIRING_TEMPLATE);
    let template = resolve_template(template_id).unwrap_or_else(|| {
        tracing::warn!(
            template = template_id,
            "unknown pairing scope template; falling back to default \"{DEFAULT_PAIRING_TEMPLATE}\""
        );
        resolve_template(DEFAULT_PAIRING_TEMPLATE)
            .expect("default pairing template is in the catalog")
    });

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
        let direction = super::policy::client_route_direction(route_id)?
            .unwrap_or(super::policy::PairingDirection::ClientToRuntime);
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
    signed_endpoint: &NetworkEndpointEntry,
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
        .unwrap_or(DEFAULT_PAIRING_TEMPLATE);
    let template = resolve_template(template_id).unwrap_or_else(|| {
        tracing::warn!(
            template = template_id,
            "unknown data-plane pairing scope template; falling back to default \"{DEFAULT_PAIRING_TEMPLATE}\""
        );
        resolve_template(DEFAULT_PAIRING_TEMPLATE)
            .expect("default pairing template is in the catalog")
    });
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
            let cols = template
                .collections
                .iter()
                .map(|&c| c.to_string())
                .collect::<BTreeSet<_>>();
            (cols, BTreeSet::new())
        };

    let filter_collections = replicator_collections
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let replicator_filter =
        data_plane_scope_filter(&template.scope, &filter_collections, peer_did, self_did);

    Ok(Some(PairingDesired {
        collections: subscription_collections,
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    }))
}

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
        Scope::ClientRoute => super::policy::resolve_template_filters(
            resolve_template(super::templates::CLIENT_TEMPLATE).expect("client template"),
            super::policy::PairingDirection::RuntimeToClient,
            signed_peer_did,
            local_did,
        ),
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

fn earned_bearer_readiness(
    desired: Option<&PairingDesired>,
    applied: &PairingApplied,
    local_did: &str,
) -> Option<(String, String, String)> {
    let desired = desired?;
    let template_id = desired
        .template_ids
        .iter()
        .filter(|id| super::templates::conversation_like(id))
        .max()?;
    if desired.replicator_addresses.len() != 1
        || applied.replicator_addresses != desired.replicator_addresses
        || applied.replicator_filter != desired.replicator_filter
    {
        return None;
    }
    let template = resolve_template(template_id)?;
    if !template
        .collections
        .iter()
        .all(|collection| desired.replicator_collections.contains(*collection))
    {
        return None;
    }
    let readiness_filter = desired.replicator_filter.get("BearerPairingReady")?;
    let Some(claimant_did) = conjunctive_string_eq(readiness_filter, "claimant_did") else {
        tracing::warn!(
            "BearerPairingReady filter has no unambiguous claimant_did equality; \
             withholding readiness"
        );
        return None;
    };
    let claimant_did = claimant_did.trim();
    if claimant_did.is_empty() {
        return None;
    }
    let expected = super::policy::resolve_template_filters(
        template,
        super::policy::PairingDirection::RuntimeToClient,
        claimant_did,
        local_did,
    );
    if expected.iter().any(|(collection, predicate)| {
        let Some(actual) = desired.replicator_filter.get(collection) else {
            return true;
        };
        if collection == "BearerPairingReady" {
            conjunctive_string_eq(actual, "claimant_did") != Some(claimant_did)
        } else {
            actual != predicate
        }
    }) {
        return None;
    }
    let address = desired
        .replicator_addresses
        .iter()
        .next()?
        .trim()
        .to_string();
    (!address.is_empty()).then(|| (claimant_did.to_string(), address, template.id.to_string()))
}

fn bearer_pairing_ready_record(
    row: &BearerPairingReadyRow,
) -> Result<Option<BearerPairingReadyRecord>> {
    let required = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let Some(issuer_did) = required(row.issuer_did.as_deref()) else {
        return Ok(None);
    };
    let Some(claimant_did) = required(row.claimant_did.as_deref()) else {
        return Ok(None);
    };
    let Some(peer_id) = required(row.peer_id.as_deref()) else {
        return Ok(None);
    };
    let Some(address) = required(row.address.as_deref()) else {
        return Ok(None);
    };
    let Some(template) = required(row.template.as_deref()) else {
        return Ok(None);
    };
    let Some(acknowledged_at) = required(row.acknowledged_at.as_deref()) else {
        return Ok(None);
    };
    let Some(issuer_sig) = required(row.issuer_sig.as_deref()) else {
        return Ok(None);
    };
    let sig = bs58::decode(issuer_sig)
        .into_vec()
        .context("decoding BearerPairingReady.issuer_sig")?;
    Ok(Some(BearerPairingReadyRecord {
        issuer_did,
        claimant_did,
        peer_id,
        address,
        template,
        acknowledged_at,
        sig,
    }))
}

pub fn bearer_pairing_ready_upsert_mutation(
    readiness_key: &str,
    record: &BearerPairingReadyRecord,
) -> String {
    let readiness_key = escape_graphql_string(readiness_key);
    let issuer_did = escape_graphql_string(&record.issuer_did);
    let claimant_did = escape_graphql_string(&record.claimant_did);
    let peer_id = escape_graphql_string(&record.peer_id);
    let address = escape_graphql_string(&record.address);
    let template = escape_graphql_string(&record.template);
    let acknowledged_at = escape_graphql_string(&record.acknowledged_at);
    let issuer_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
    format!(
        r#"mutation {{
            delete_BearerPairingReady(
                filter: {{ readiness_key: {{ _eq: "{readiness_key}" }} }}
            ) {{ _docID }}
            upsert_BearerPairingReady(
                filter: {{ readiness_key: {{ _eq: "{readiness_key}" }} }},
                add: {{
                    readiness_key: "{readiness_key}",
                    issuer_did: "{issuer_did}",
                    claimant_did: "{claimant_did}",
                    peer_id: "{peer_id}",
                    address: "{address}",
                    template: "{template}",
                    acknowledged_at: "{acknowledged_at}",
                    issuer_sig: "{issuer_sig}"
                }},
                update: {{
                    peer_id: "{peer_id}",
                    address: "{address}",
                    template: "{template}",
                    acknowledged_at: "{acknowledged_at}",
                    issuer_sig: "{issuer_sig}"
                }}
            ) {{ _docID }}
        }}"#
    )
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
