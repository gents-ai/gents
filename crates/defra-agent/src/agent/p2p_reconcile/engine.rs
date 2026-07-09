//! Runtime pairing reconcile engine.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::network::{GraphqlNetworkStore, NetworkEndpointEntry, NetworkStore};
use super::templates::{
    resolve_template, scope_filter, Delivery, DidSource, PairingFilters, Scope,
    APP_COLLECTIONS_TEMPLATE,
};
use super::{
    compute_owned_pairing_diff, DiffOp, EmbeddedRemoteP2pAdmin, PairingActual, PairingApplied,
    PairingDesired, RemoteP2pAdmin,
};

pub const PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTickOutcome {
    pub peer_id: String,
    pub ops_applied: Vec<DiffOp>,
    /// Existing subagent replicators force-reinstalled after a disconnected
    /// peer is dialed again. Reinstall performs a bounded full replay without
    /// authoring same-value AgentRequest mutations.
    pub replayed_replicators: Vec<String>,
    pub desired_read_failed: bool,
}

#[async_trait]
pub trait PairingStateStore: Send + Sync {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>>;

    async fn load_applied(&self, peer_id: &str) -> Result<PairingApplied>;

    async fn save_applied(&self, peer_id: &str, applied: &PairingApplied) -> Result<()>;

    async fn delete_applied(&self, peer_id: &str) -> Result<()>;

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
    let desired = match store.load_desired(peer_id).await {
        Ok(desired) => desired,
        Err(error) => {
            tracing::warn!(
                peer_id,
                error = %error,
                "pairing desired state read failed; skipping reconcile tick"
            );
            return Ok(PairingTickOutcome {
                peer_id: peer_id.to_string(),
                ops_applied: Vec::new(),
                replayed_replicators: Vec::new(),
                desired_read_failed: true,
            });
        }
    };
    let desired_state = desired.clone().unwrap_or_default();
    let mut reconnected = false;
    if desired_state.has_wiring() && !desired_state.replicator_addresses.is_empty() {
        // Dial only when the peer is not already connected — the Lean
        // `PairingReconcile.Transition.dial`/`dialFailed` premises both require
        // `connected = false`; a connected peer proceeds straight to the
        // reconcile ops. A redundant redial is not merely wasted work: on Linux
        // it can time out even though the connection is healthy, and a dial
        // failure aborts this tick before the diff below runs — leaving an
        // already-paired peer permanently unable to pick up new desired state
        // (e.g. the filtered conversation data-plane replicator on top of an
        // applied control-plane pairing).
        if peer_already_active(admin, peer_id).await {
            tracing::debug!(peer_id, "pairing peer already connected; skipping redial");
        } else {
            let addresses = desired_state
                .replicator_addresses
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            admin
                .connect(&addresses)
                .await
                .context("connect pairing peer")?;
            reconnected = true;
        }
    }
    let mut applied = store.load_applied(peer_id).await?;
    let actual = read_actual(admin).await?;

    // All three collection sets are in name-space: `desired_state` carries names,
    // `read_actual` reverse-resolves the remote subscription ids back to names, and
    // the persisted `applied` row records names. The diff therefore compares like
    // with like, and `PeerPairingApplied.collections` stays human-readable for CLI
    // display and health (review Finding #1).
    let ops = compute_owned_pairing_diff(&desired_state, &actual.state, &applied);
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
        update_applied_after_success(&mut applied, &op, &desired_state);
        persist_applied(store, peer_id, &applied).await?;
        ops_applied.push(op);
    }

    if desired.is_none() && !applied.is_empty() {
        store.delete_applied(peer_id).await?;
    }

    Ok(PairingTickOutcome {
        peer_id: peer_id.to_string(),
        ops_applied,
        replayed_replicators,
        desired_read_failed: false,
    })
}

/// True when `peer_id` already has a live connection according to
/// `active_peers`. Entries are either bare peer ids or the dial address
/// recorded for the peer (the embedded adapter returns whichever it has), so
/// extract the peer id from each entry rather than comparing verbatim —
/// mirroring the desktop supervisor's `is_connected_peer`. A failed read
/// degrades to "not connected": the tick dials exactly as it did before this
/// check existed.
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
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut replay_connections = BTreeMap::new();

    sweep_pairings(&admin, &store, &mut replay_connections).await?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                sweep_pairings_logged(&admin, &store, &mut replay_connections).await;
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
                sweep_pairings_logged(&admin, &store, &mut replay_connections).await;
            }
        }
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
) {
    if let Err(error) = sweep_pairings(admin, store, replay_connections).await {
        tracing::warn!(error = %error, "pairing reconciler sweep failed; retrying on next tick");
    }
}

async fn sweep_pairings(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    replay_connections: &mut BTreeMap<String, bool>,
) -> Result<()> {
    // Amortize the membership-materializable computation across the whole sweep
    // (avoids re-verifying every signature per peer). Non-fatal on failure: the
    // per-peer gate falls back to a live read.
    store.begin_sweep().await?;
    for peer_id in store.list_peer_ids().await? {
        let active_before = peer_already_active(admin, &peer_id).await;
        // Replay once on daemon startup, and on an inactive -> active edge even
        // when the remote peer established the connection first. The latter is
        // essential for bidirectional pairing: relying only on our own dial
        // would repair whichever direction won the reconnect race and could
        // leave the opposite direction stale beyond its request-write cap.
        let force_replay = replay_connections
            .get(&peer_id)
            .is_none_or(|was_active| !was_active && active_before);
        let tick_succeeded =
            match reconcile_peer_tick_with_replay(admin, store, &peer_id, force_replay).await {
                Ok(outcome) => {
                    if outcome.desired_read_failed {
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
                    true
                }
                Err(error) => {
                    tracing::warn!(
                        peer_id = %peer_id,
                        error = ?error,
                        "pairing reconcile tick failed"
                    );
                    false
                }
            };
        let active_after = peer_already_active(admin, &peer_id).await;
        // Keep replay pending after any transient delete/reinstall failure,
        // even if the transport itself is connected. The next sweep then
        // retries the bounded replay instead of mistaking connectivity for a
        // successfully repaired data plane.
        replay_connections.insert(peer_id.clone(), tick_succeeded && active_after);
    }
    Ok(())
}

struct ActualSnapshot {
    state: PairingActual,
    replicator_ids_by_addr: BTreeMap<String, String>,
    replicator_collections_by_addr: BTreeMap<String, Vec<String>>,
}

async fn read_actual(admin: &dyn RemoteP2pAdmin) -> Result<ActualSnapshot> {
    // `list_p2p_collections` returns the remote subscription set in collection-*id*
    // space, but desired/operator state and the persisted `PeerPairingApplied` row
    // are in collection-*name* space (the human-readable, observable contract). The
    // reconcile diff must compare both sides in one space, so normalize the read
    // boundary by reverse-resolving each id back to its name. Every collection the
    // remote is subscribed to is one this node also subscribed and therefore has
    // locally, so its name is always resolvable; if an id somehow can't be resolved
    // we degrade gracefully (keep the id and warn) rather than churn or panic.
    let mut collections = BTreeSet::new();
    for id in admin
        .list_p2p_collections()
        .await
        .context("list remote P2P collections")?
    {
        match admin
            .resolve_collection_name(&id)
            .await
            .with_context(|| format!("resolve collection name for id {id}"))?
        {
            Some(name) => {
                collections.insert(name);
            }
            None => {
                tracing::warn!(
                    collection_id = %id,
                    "remote P2P collection id has no local name; keeping the id in \
                     the actual set"
                );
                collections.insert(id);
            }
        }
    }
    let remote_replicators = admin
        .list_replicators()
        .await
        .context("list remote P2P replicators")?;
    let replicator_addresses = remote_replicators
        .iter()
        .filter_map(|replicator| replicator.address.clone())
        .collect::<BTreeSet<_>>();
    let replicator_ids_by_addr = remote_replicators
        .iter()
        .filter_map(|replicator| Some((replicator.address.clone()?, replicator.id.clone()?)))
        .collect::<BTreeMap<_, _>>();
    let replicator_collections_by_addr = remote_replicators
        .into_iter()
        .filter_map(|replicator| Some((replicator.address?, replicator.collections)))
        .collect::<BTreeMap<_, _>>();

    // The diff compares the replicator's carried collection set in *name*
    // space (part of the replicator identity, Lean
    // `collections_change_forces_reinstall`), but the transport reports it in
    // id space — reverse-resolve like the subscription set above. An
    // unresolvable token is kept raw at debug (unlike subscriptions, some
    // adapters report names here already, so raw is not necessarily wrong).
    let mut replicator_collections = BTreeMap::new();
    for (address, ids) in &replicator_collections_by_addr {
        let mut names = BTreeSet::new();
        for id in ids {
            match admin
                .resolve_collection_name(id)
                .await
                .with_context(|| format!("resolve replicator collection name for id {id}"))?
            {
                Some(name) => {
                    names.insert(name);
                }
                None => {
                    tracing::debug!(
                        collection = %id,
                        address = %address,
                        "replicator collection token has no local name; keeping it raw"
                    );
                    names.insert(id.clone());
                }
            }
        }
        replicator_collections.insert(address.clone(), names);
    }

    Ok(ActualSnapshot {
        state: PairingActual {
            collections,
            replicator_addresses,
            replicator_collections,
        },
        replicator_ids_by_addr,
        replicator_collections_by_addr,
    })
}

async fn apply_op(
    admin: &dyn RemoteP2pAdmin,
    op: &DiffOp,
    desired: &PairingDesired,
    actual: &ActualSnapshot,
) -> Result<()> {
    match op {
        // The diff runs entirely in collection-*name* space, and the admin
        // subscribes/unsubscribes by name, so the op token is already the name —
        // pass it straight through.
        DiffOp::InstallCollection(collection) => admin
            .add_p2p_collections(std::slice::from_ref(collection))
            .await
            .with_context(|| format!("install P2P collection {collection}")),
        DiffOp::TeardownCollection(collection) => admin
            .delete_p2p_collections(std::slice::from_ref(collection))
            .await
            .with_context(|| format!("teardown P2P collection {collection}")),
        DiffOp::InstallReplicator(address) => {
            let addresses = vec![address.clone()];
            // The replicator carries the template's collection set, which is
            // independent of the subscription set (`collections`): a `Push`
            // template subscribes to nothing but still replicates the full set.
            // Legacy rows with no explicit replicator set fall back to the
            // subscription collections (the same effective set the diff keys
            // the replicator identity on).
            let collections = desired
                .effective_replicator_collections()
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            admin
                .add_replicator(&addresses, &collections, &desired.replicator_filter)
                .await
                .with_context(|| format!("install P2P replicator {address}"))?;
            if desired.uses_subagent_template() {
                tracing::debug!(
                    address = %address,
                    templates = ?desired.template_ids,
                    "subagent pairing replicator installed with initial full replay"
                );
            }
            Ok(())
        }
        DiffOp::TeardownReplicator(address) => {
            let id = actual
                .replicator_ids_by_addr
                .get(address)
                .map(String::as_str)
                .unwrap_or(address.as_str());
            let collections = actual
                .replicator_collections_by_addr
                .get(address)
                .cloned()
                .filter(|collections| !collections.is_empty())
                .unwrap_or_else(|| desired.collections.iter().cloned().collect());
            admin
                .delete_replicator(id, &collections)
                .await
                .with_context(|| format!("teardown P2P replicator {address}"))
        }
    }
}

async fn replay_replicator_after_reconnect(
    admin: &dyn RemoteP2pAdmin,
    address: &str,
    desired: &PairingDesired,
    actual: &ActualSnapshot,
) -> Result<()> {
    let id = actual
        .replicator_ids_by_addr
        .get(address)
        .map(String::as_str)
        .unwrap_or(address);
    let old_collections = actual
        .replicator_collections_by_addr
        .get(address)
        .cloned()
        .filter(|collections| !collections.is_empty())
        .unwrap_or_else(|| {
            desired
                .effective_replicator_collections()
                .iter()
                .cloned()
                .collect()
        });
    admin
        .delete_replicator(id, &old_collections)
        .await
        .with_context(|| format!("remove P2P replicator {address} for reconnect replay"))?;

    let addresses = vec![address.to_string()];
    let collections = desired
        .effective_replicator_collections()
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    admin
        .add_replicator(&addresses, &collections, &desired.replicator_filter)
        .await
        .with_context(|| format!("reinstall P2P replicator {address} for reconnect replay"))?;
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
            // The filter is part of the replicator's applied identity: record
            // the desired filter that was just installed so a later change is
            // detected as divergence (Lean `filter_change_forces_reinstall`).
            applied.replicator_filter = desired.replicator_filter.clone();
        }
        DiffOp::TeardownReplicator(address) => {
            applied.replicator_addresses.remove(address);
            // Once no managed replicator remains, the recorded filter identity
            // is meaningless — clear it so an empty applied state is canonical.
            if applied.replicator_addresses.is_empty() {
                applied.replicator_filter = PairingFilters::default();
            }
        }
    }
}

async fn persist_applied(
    store: &dyn PairingStateStore,
    peer_id: &str,
    applied: &PairingApplied,
) -> Result<()> {
    if applied.is_empty() {
        store.delete_applied(peer_id).await
    } else {
        store.save_applied(peer_id, applied).await
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
}

impl GraphqlPairingStateStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self {
            node,
            identity,
            materializable_cache: Arc::new(Mutex::new(None)),
        }
    }

    async fn data_plane_materialized_entry(
        &self,
        peer_id: &str,
    ) -> Result<Option<NetworkEndpointEntry>> {
        // Prefer the per-sweep cache; fall back to a live read when it is absent
        // (first read, outside a sweep, or after a refresh failure).
        let cached = self.materializable_cache.lock().unwrap().clone();
        let entries = match cached {
            Some(entries) => entries,
            None => {
                let network = GraphqlNetworkStore::new(self.node.clone(), self.identity.clone());
                network.load_materializable_entries().await?
            }
        };
        Ok(
            super::network::materializable_entry_for_peer(&entries, peer_id, self.identity.did())
                .cloned(),
        )
    }
}

#[async_trait]
impl PairingStateStore for GraphqlPairingStateStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        let raw_peer_id = peer_id.to_string();
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    agent_did
                    replicator_addresses
                    template
                }}
                DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    agent_did
                    collections
                    replicator_addresses
                    template
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query pairing desired state")?;
        let base = first_row::<PairingStateRow>(&response, "PeerPairingDesired")?
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
        Ok(merge_layered_desired(base, data_plane))
    }

    async fn load_applied(&self, peer_id: &str) -> Result<PairingApplied> {
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    collections
                    replicator_addresses
                    replicator_filter
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query PeerPairingApplied")?;
        Ok(
            first_row::<PairingStateRow>(&response, "PeerPairingApplied")?
                .map(|row| PairingApplied {
                    collections: row.collections.unwrap_or_default().into_iter().collect(),
                    replicator_addresses: row
                        .replicator_addresses
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                    replicator_filter: decode_replicator_filter(row.replicator_filter.as_deref()),
                })
                .unwrap_or_default(),
        )
    }

    async fn save_applied(&self, peer_id: &str, applied: &PairingApplied) -> Result<()> {
        let peer_id = escape_graphql_string(peer_id);
        let collections = graphql_nullable_string_array(&applied.collections);
        let replicator_addresses = graphql_nullable_string_array(&applied.replicator_addresses);
        let replicator_filter = graphql_nullable_filter_literal(&applied.replicator_filter);
        let now = escape_graphql_string(
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let mutation = format!(
            r#"mutation {{
                upsert_PeerPairingApplied(
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
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "upsert PeerPairingApplied")
    }

    async fn delete_applied(&self, peer_id: &str) -> Result<()> {
        let peer_id = escape_graphql_string(peer_id);
        let mutation = format!(
            r#"mutation {{
                delete_PeerPairingApplied(
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete PeerPairingApplied")
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
        Ok(())
    }
}

#[derive(Deserialize)]
struct PairingStateRow {
    #[serde(default)]
    agent_did: Option<String>,
    collections: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    replicator_filter: Option<String>,
}

/// The default scope template applied to rows that carry no `template` (e.g.
/// rows written before the field existed). Mirrors the migration backfill.
pub const DEFAULT_PAIRING_TEMPLATE: &str = "conversation";

#[derive(Deserialize)]
struct PeerIdRow {
    peer_id: String,
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

    // Template-driven resolution: the template is authoritative for the
    // collection set, the per-peer scope filter, and the delivery mode. Rows
    // without a `template` (pre-migration) default to `conversation`, matching
    // the migration backfill.
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

    // app-collections is a data-plane-only (bring-your-own) policy: a
    // control-plane / PeerPairingDesired row cannot supply row collections, so
    // it would resolve to empty wiring yet has_wiring() would be true (addresses
    // present). Refuse to wire it. Soft-skip (Ok(None) + warn) so a raw-GraphQL
    // row cannot install an empty-collection replicator.
    if template.id == APP_COLLECTIONS_TEMPLATE {
        tracing::warn!(
            "PeerPairingDesired names the app-collections template, which is \
             data-plane-only and supplies no collections here; skipping (no wiring)"
        );
        return Ok(None);
    }

    // The scope filter value is the row's agent DID. For network-control rows
    // this is the remote member DID; for data-plane rows the loader first
    // sanitizes it to this node's DID so the node pushes only its own docs. A
    // peer-DID-scoped template with a blank agent_did cannot be honored: it would
    // build an `agent_did == ""` predicate (matches nothing) or, worse, an
    // unscoped replicator. Refuse the row and skip this peer (caught per-peer by
    // the sweep), mirroring the discovery-side skip of blank-DID registry entries.
    let peer_did = row.agent_did.as_deref().map(str::trim).unwrap_or_default();
    if peer_did.is_empty() && scope_requires_peer_did(&template.scope) {
        anyhow::bail!(
            "pairing row for peer-DID-dependent template {template_id:?} has a blank \
             agent_did; refusing to install an unscoped replicator (skipping peer)"
        );
    }
    let replicator_collections = template
        .collections
        .iter()
        .map(|&c| c.to_string())
        .collect::<BTreeSet<_>>();
    let replicator_filter =
        scope_filter(&template.scope, template.collections, peer_did, local_did);

    let subscription_collections = match template.delivery {
        // Push: never subscribe — the filtered replicator is the only channel,
        // so the unfiltered collection never gossips.
        Delivery::Push => BTreeSet::new(),
        // Replicate: subscribe to the whole collection set.
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

    // app-collections (bring-your-own): the row supplies the collection set; the
    // template supplies only scope (Unscoped) + delivery (Replicate). A blank set
    // is malformed input — SOFT-SKIP this layer (Ok(None) + warn) rather than
    // bail, so a bad app row never fails the whole peer's desired load and stalls
    // a co-existing control pairing (reconcile_peer_tick desired_read_failed).
    let (replicator_collections, subscription_collections): (BTreeSet<String>, BTreeSet<String>) =
        if template.id == APP_COLLECTIONS_TEMPLATE {
            let row_cols = row
                .collections
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect::<BTreeSet<_>>();
            if row_cols.is_empty() {
                tracing::warn!(
                    peer_id = %signed_endpoint.peer_id,
                    "app-collections DataPlanePairingDesired has no non-blank collections; \
                     skipping this data-plane layer (control pairing unaffected)"
                );
                return Ok(None);
            }
            // Replicate: subscribe to the same set so the merged doc is observable.
            (row_cols.clone(), row_cols)
        } else {
            // Legacy / template-driven data-plane rows: unchanged. Template
            // collections drive the replicator; no subscription (push channel).
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
    }
}

fn data_plane_scope_requires_signed_peer_did(scope: &Scope) -> bool {
    match scope {
        Scope::PeerDid { .. } | Scope::Unscoped => false,
        Scope::PerCollection(rules) => rules
            .iter()
            .any(|rule| matches!(rule.source, DidSource::PeerDid)),
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
            .map(|&col| {
                (
                    col.to_string(),
                    super::templates::FilterPredicate {
                        field: (*field).to_string(),
                        value: local_did.to_string(),
                    },
                )
            })
            .collect(),
        Scope::Unscoped => BTreeMap::new(),
        Scope::PerCollection(rules) => rules
            .iter()
            .map(|rule| {
                let value = match rule.source {
                    DidSource::LocalDid => local_did,
                    DidSource::PeerDid => signed_peer_did,
                };
                (
                    rule.collection.to_string(),
                    super::templates::FilterPredicate {
                        field: rule.field.to_string(),
                        value: value.to_string(),
                    },
                )
            })
            .collect(),
    }
}

/// Merge Layer-1 control-plane desired state with optional Layer-2 data-plane
/// desired state for the same peer.
///
/// Most data-plane templates are delivered by filtered push replicators, so
/// their collections extend only the per-peer replicator set — the subscription
/// set is cleared so conversation data never gossips unfiltered. Exception:
/// the `app-collections` (bring-your-own Unscoped/Replicate) policy preserves
/// its subscription set so the merged doc is observable on both sides.
pub fn merge_layered_desired(
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    // Layer-2 desired rows add data-plane collections to the per-peer
    // replicator, not to the subscription set — EXCEPT the app-collections
    // (bring-your-own) policy, which is a whole-collection Replicate that must
    // subscribe on both sides for the merged doc to be observable. All other
    // data-plane layers keep the clear so conversation data never gossips
    // unfiltered.
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
            left.replicator_addresses.extend(right.replicator_addresses);
            left.replicator_collections
                .extend(right.replicator_collections);
            left.replicator_filter.extend(right.replicator_filter);
            left.template_ids.extend(right.template_ids);
            Some(left)
        }
    }
}

fn ensure_no_errors(response: &QueryResponse, label: &str) -> Result<()> {
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}

fn first_row<T>(response: &QueryResponse, field: &str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    Ok(rows::<T>(response, field)?.into_iter().next())
}

fn rows<T>(response: &QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = response.data.as_ref().and_then(|data| data.get(field)) else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).with_context(|| format!("decode {field} rows"))
}

/// Serialize the per-pairing scope filter to a GraphQL String literal (JSON),
/// emitting `null` for the unfiltered (empty) case so the column is never an
/// empty-list literal. The JSON round-trips through `decode_replicator_filter`.
fn graphql_nullable_filter_literal(filter: &PairingFilters) -> String {
    if filter.is_empty() {
        return "null".to_string();
    }
    let json = serde_json::to_string(filter).unwrap_or_default();
    format!(r#""{}""#, escape_graphql_string(&json))
}

/// Decode the persisted scope filter String (JSON) back into `PairingFilters`.
/// Missing/empty/malformed values decode to an empty (unfiltered) filter.
fn decode_replicator_filter(value: Option<&str>) -> PairingFilters {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return PairingFilters::default();
    };
    serde_json::from_str(raw).unwrap_or_else(|error| {
        tracing::warn!(
            error = %error,
            "PeerPairingApplied.replicator_filter failed to decode; treating as unfiltered"
        );
        PairingFilters::default()
    })
}

fn graphql_nullable_string_array(values: &BTreeSet<String>) -> String {
    if values.is_empty() {
        return "null".to_string();
    }
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::anyhow;

    use super::*;
    use crate::agent::p2p_reconcile::{
        RemoteP2pAdminError, RemoteP2pAdminResult, RemoteReplicator,
    };

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn one_filter(collection: &str, field: &str, value: &str) -> PairingFilters {
        let mut filters = PairingFilters::new();
        filters.insert(
            collection.to_string(),
            crate::agent::p2p_reconcile::FilterPredicate {
                field: field.to_string(),
                value: value.to_string(),
            },
        );
        filters
    }

    #[test]
    fn merge_desired_unions_control_and_data_plane_state() {
        let control = PairingDesired {
            collections: set(&["AgentNetwork", "NetworkMembership"]),
            replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
            replicator_collections: set(&["AgentNetwork", "NetworkMembership"]),
            replicator_filter: PairingFilters::new(),
            template_ids: BTreeSet::new(),
        };
        let data = PairingDesired {
            collections: set(&["AgentRequest"]),
            replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
            replicator_collections: set(&["AgentRequest"]),
            replicator_filter: one_filter("AgentRequest", "agent_did", "did:key:a"),
            template_ids: BTreeSet::new(),
        };

        let merged = merge_layered_desired(Some(control), Some(data)).expect("merged desired");
        assert_eq!(
            merged.replicator_collections,
            set(&["AgentNetwork", "NetworkMembership", "AgentRequest"])
        );
        assert_eq!(
            merged.collections,
            set(&["AgentNetwork", "NetworkMembership"]),
            "data-plane collections must not expand the subscription set"
        );
        assert_eq!(
            merged.replicator_addresses,
            set(&["/ip4/1/tcp/1/p2p/peer-a"])
        );
        assert_eq!(
            merged
                .replicator_filter
                .get("AgentRequest")
                .map(|filter| (filter.field.as_str(), filter.value.as_str())),
            Some(("agent_did", "did:key:a"))
        );
        assert!(!merged.replicator_filter.contains_key("AgentNetwork"));
    }

    #[test]
    fn data_plane_only_desired_is_replicator_only() {
        let data = PairingDesired {
            collections: set(&["AgentRequest"]),
            replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
            replicator_collections: set(&["AgentRequest"]),
            replicator_filter: one_filter("AgentRequest", "agent_did", "did:key:a"),
            template_ids: BTreeSet::new(),
        };

        let merged = merge_layered_desired(None, Some(data)).expect("data-plane desired");
        assert!(
            merged.collections.is_empty(),
            "data-plane-only desired must not subscribe to conversation collections"
        );
        assert_eq!(merged.replicator_collections, set(&["AgentRequest"]));
        assert!(merged.replicator_filter.contains_key("AgentRequest"));
    }

    #[test]
    fn data_plane_desired_uses_signed_endpoint_address_and_self_did() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:peer-b".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let desired = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: None,
                collections: None,
                replicator_addresses: Some(vec!["/ip4/192.0.2.1/tcp/9999/p2p/forged".to_string()]),
                template: Some("conversation".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:self",
        )
        .expect("data-plane desired")
        .expect("some data-plane layer");

        assert_eq!(
            desired.replicator_addresses,
            set(&["/ip4/127.0.0.1/tcp/4001/p2p/peer-b"])
        );
        assert_eq!(
            desired
                .replicator_filter
                .get("AgentRequest")
                .map(|filter| (filter.field.as_str(), filter.value.as_str())),
            Some(("agent_did", "did:key:self"))
        );
    }

    #[test]
    fn data_plane_subagent_coordinator_uses_signed_peer_for_targeted_bridge() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:host".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let desired = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:coord".to_string()),
                collections: None,
                replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
                template: Some("subagent-coordinator".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:coord",
        )
        .expect("data-plane coordinator desired")
        .expect("some data-plane layer");

        assert_eq!(
            desired
                .replicator_filter
                .get("AgentRequest")
                .map(|filter| (filter.field.as_str(), filter.value.as_str())),
            Some(("agent_did", "did:key:coord"))
        );
        assert_eq!(
            desired
                .replicator_filter
                .get("AgentToolCall")
                .map(|filter| (filter.field.as_str(), filter.value.as_str())),
            Some(("spawn_target_did", "did:key:host"))
        );
    }

    #[test]
    fn data_plane_subagent_host_scopes_child_conversation_to_local_host() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-a".to_string(),
            agent_did: "did:key:coord".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-a".to_string(),
        };
        let desired = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:host".to_string()),
                collections: None,
                replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
                template: Some("subagent-host".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:host",
        )
        .expect("data-plane host desired")
        .expect("some data-plane layer");

        assert_eq!(desired.replicator_filter.len(), 8);
        for predicate in desired.replicator_filter.values() {
            assert_eq!(predicate.field, "agent_did");
            assert_eq!(predicate.value, "did:key:host");
        }
    }

    #[test]
    fn data_plane_desired_rejects_foreign_agent_did_scope() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:peer-b".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let error = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:someone-else".to_string()),
                collections: None,
                replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
                template: Some("conversation".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:self",
        )
        .expect_err("foreign data-plane scope should be rejected");

        assert!(error.to_string().contains("foreign DID"));
    }

    /// Deterministic name → collection-id transform used by `MockAdmin`.
    ///
    /// The real P2P adapter resolves a collection *name* to a distinct collection
    /// *id* when subscribing (`add_collections`) and returns ids from
    /// `get_collections`. The mock must mirror that distinctness — echoing the
    /// name back (id == name) would hide the very id-space mismatch this engine
    /// must reconcile (review Finding #1). The prefix guarantees id != name.
    fn mock_collection_id(name: &str) -> String {
        format!("col_{name}_id")
    }

    struct MockStore {
        desired: Mutex<Result<Option<PairingDesired>, String>>,
        applied: Mutex<PairingApplied>,
        saved: Mutex<Vec<PairingApplied>>,
        deleted: Mutex<usize>,
    }

    impl Default for MockStore {
        fn default() -> Self {
            Self {
                desired: Mutex::new(Ok(None)),
                applied: Mutex::new(PairingApplied::default()),
                saved: Mutex::new(Vec::new()),
                deleted: Mutex::new(0),
            }
        }
    }

    impl MockStore {
        fn with_desired(desired: Option<PairingDesired>) -> Self {
            Self {
                desired: Mutex::new(Ok(desired)),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl PairingStateStore for MockStore {
        async fn load_desired(&self, _peer_id: &str) -> Result<Option<PairingDesired>> {
            self.desired
                .lock()
                .unwrap()
                .clone()
                .map_err(|message| anyhow!(message))
        }

        async fn load_applied(&self, _peer_id: &str) -> Result<PairingApplied> {
            Ok(self.applied.lock().unwrap().clone())
        }

        async fn save_applied(&self, _peer_id: &str, applied: &PairingApplied) -> Result<()> {
            *self.applied.lock().unwrap() = applied.clone();
            self.saved.lock().unwrap().push(applied.clone());
            Ok(())
        }

        async fn delete_applied(&self, _peer_id: &str) -> Result<()> {
            *self.applied.lock().unwrap() = PairingApplied::default();
            *self.deleted.lock().unwrap() += 1;
            Ok(())
        }

        async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
            Ok(set(&["peer-a"]))
        }
    }

    #[derive(Default)]
    struct MockAdmin {
        collections: Mutex<BTreeSet<String>>,
        replicators: Mutex<BTreeMap<String, RemoteReplicator>>,
        emitted: Mutex<Vec<DiffOp>>,
        connects: Mutex<Vec<Vec<String>>>,
        /// Filters recorded per `add_replicator` call: (addresses, filters).
        recorded_filters: Mutex<Vec<(Vec<String>, PairingFilters)>>,
        /// Entries returned by `active_peers` (bare peer ids or dial addresses,
        /// like the real adapters).
        active: Mutex<Vec<String>>,
        /// When set, `active_peers` fails, exercising the degraded-read path.
        fail_active_peers: bool,
        /// When set, `connect` fails after recording the call — modeling the
        /// Linux redial-timeout that motivated the active-peer gate.
        fail_connect: bool,
    }

    #[async_trait]
    impl RemoteP2pAdmin for MockAdmin {
        async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
            if self.fail_active_peers {
                return Err(RemoteP2pAdminError::RpcError("active_peers down".into()));
            }
            Ok(self.active.lock().unwrap().clone())
        }

        async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
            self.connects.lock().unwrap().push(addresses.to_vec());
            if self.fail_connect {
                return Err(RemoteP2pAdminError::RpcTimeout);
            }
            Ok(())
        }

        async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
            Ok(self.replicators.lock().unwrap().values().cloned().collect())
        }

        async fn add_replicator(
            &self,
            addresses: &[String],
            collections: &[String],
            filters: &PairingFilters,
        ) -> RemoteP2pAdminResult<()> {
            self.recorded_filters
                .lock()
                .unwrap()
                .push((addresses.to_vec(), filters.clone()));
            for address in addresses {
                // Like the real adapter, the transport records the carried
                // collection set in *id* space; `read_actual` reverse-resolves
                // it to names for the identity comparison.
                self.replicators.lock().unwrap().insert(
                    address.clone(),
                    RemoteReplicator {
                        id: Some(format!("id-{address}")),
                        collections: collections.iter().map(|c| mock_collection_id(c)).collect(),
                        address: Some(address.clone()),
                    },
                );
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
            let key = self
                .replicators
                .lock()
                .unwrap()
                .iter()
                .find_map(|(address, replicator)| {
                    (replicator.id.as_deref() == Some(id) || address == id).then(|| address.clone())
                });
            if let Some(key) = key {
                self.replicators.lock().unwrap().remove(&key);
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::TeardownReplicator(key));
            }
            Ok(())
        }

        // The subscription set is stored in *id*-space, mirroring the real
        // adapter: `add_p2p_collections` receives names and persists the resolved
        // id; `list_p2p_collections` returns those ids. `resolve_collection_id`
        // maps name → id with a distinct prefix so id == name never holds.
        async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(self.collections.lock().unwrap().iter().cloned().collect())
        }

        async fn resolve_collection_id(&self, name: &str) -> RemoteP2pAdminResult<Option<String>> {
            Ok(Some(mock_collection_id(name)))
        }

        async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>> {
            // Invert `mock_collection_id`: "col_<name>_id" -> "<name>".
            Ok(id
                .strip_prefix("col_")
                .and_then(|rest| rest.strip_suffix("_id"))
                .map(str::to_string))
        }

        async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
            for collection in collections {
                // `collection` is a name; the adapter subscribes by id, so the
                // stored token is the resolved id.
                self.collections
                    .lock()
                    .unwrap()
                    .insert(mock_collection_id(collection));
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::InstallCollection(collection.clone()));
            }
            Ok(())
        }

        async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
            for collection in collections {
                self.collections
                    .lock()
                    .unwrap()
                    .remove(&mock_collection_id(collection));
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::TeardownCollection(collection.clone()));
            }
            Ok(())
        }

        async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
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
    async fn read_failure_noops_without_remote_reads() {
        let store = MockStore {
            desired: Mutex::new(Err("boom".into())),
            ..Default::default()
        };
        let admin = MockAdmin::default();

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert!(outcome.desired_read_failed);
        assert!(outcome.ops_applied.is_empty());
        assert!(admin.emitted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn install_updates_applied_after_success() {
        let store = MockStore::with_desired(Some(PairingDesired {
            collections: set(&["c1"]),
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }));
        let admin = MockAdmin::default();

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        // The subscription op and persisted Applied are in collection-*name*
        // space (the observable contract); the replicator path stays in address
        // space. The mock still stores a distinct id internally, but `read_actual`
        // reverse-resolves it to the name.
        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::InstallCollection("c1".into()),
                DiffOp::InstallReplicator("addr1".into())
            ]
        );
        assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr1"]]);
        assert_eq!(
            *store.applied.lock().unwrap(),
            PairingApplied {
                collections: set(&["c1"]),
                replicator_addresses: set(&["addr1"]),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn reconnect_force_replays_converged_subagent_replicator() {
        let filter = one_filter("AgentRequest", "agent_did", "did:key:local-owner");
        let store = MockStore::with_desired(Some(PairingDesired {
            replicator_addresses: set(&["addr1"]),
            replicator_collections: set(&["AgentRequest"]),
            replicator_filter: filter.clone(),
            template_ids: set(&["subagent-host"]),
            ..Default::default()
        }));
        *store.applied.lock().unwrap() = PairingApplied {
            replicator_addresses: set(&["addr1"]),
            replicator_filter: filter,
            ..Default::default()
        };
        let admin = MockAdmin::default();
        admin.replicators.lock().unwrap().insert(
            "addr1".into(),
            RemoteReplicator {
                id: Some("id-addr1".into()),
                collections: vec![mock_collection_id("AgentRequest")],
                address: Some("addr1".into()),
            },
        );

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("reconnect replay tick");

        assert!(
            outcome.ops_applied.is_empty(),
            "topology was already converged"
        );
        assert_eq!(outcome.replayed_replicators, vec!["addr1"]);
        assert_eq!(
            *admin.emitted.lock().unwrap(),
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ],
            "reconnect must force one bounded full replay"
        );
    }

    #[tokio::test]
    async fn inbound_reconnect_force_replays_without_owner_redial() {
        let filter = one_filter("AgentRequest", "agent_did", "did:key:local-owner");
        let store = MockStore::with_desired(Some(PairingDesired {
            replicator_addresses: set(&["addr1"]),
            replicator_collections: set(&["AgentRequest"]),
            replicator_filter: filter.clone(),
            template_ids: set(&["subagent-host"]),
            ..Default::default()
        }));
        *store.applied.lock().unwrap() = PairingApplied {
            replicator_addresses: set(&["addr1"]),
            replicator_filter: filter,
            ..Default::default()
        };
        let admin = MockAdmin::default();
        admin.active.lock().unwrap().push("peer-a".into());
        admin.replicators.lock().unwrap().insert(
            "addr1".into(),
            RemoteReplicator {
                id: Some("id-addr1".into()),
                collections: vec![mock_collection_id("AgentRequest")],
                address: Some("addr1".into()),
            },
        );

        let outcome = reconcile_peer_tick_with_replay(&admin, &store, "peer-a", true)
            .await
            .expect("inbound reconnect replay tick");

        assert!(admin.connects.lock().unwrap().is_empty());
        assert_eq!(outcome.replayed_replicators, vec!["addr1"]);
        assert_eq!(
            *admin.emitted.lock().unwrap(),
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
    }

    /// Regression for the Linux demo `pair` hang at "waiting for conversation
    /// data-plane replicators": the tick used to dial the desired replicator
    /// addresses unconditionally, so a redial of an ALREADY-connected peer that
    /// timed out aborted the tick before the diff ran — the applied
    /// control-plane pairing never got upgraded with the filtered conversation
    /// data-plane replicator (`PeerPairingApplied.replicator_filter` stayed
    /// null forever). An active peer must skip the redial and still reconcile.
    #[tokio::test]
    async fn active_peer_skips_redial_and_upgrades_data_plane_replicator() {
        let conversation_filter = one_filter("AgentRequest", "agent_did", "did:key:host");
        // Desired now includes the conversation data plane: same address, new
        // collection, and a scoped filter (identity change ⇒ reinstall).
        let store = MockStore::with_desired(Some(PairingDesired {
            collections: set(&["AgentNetwork", "AgentRequest"]),
            replicator_addresses: set(&["addr1"]),
            replicator_filter: conversation_filter.clone(),
            ..Default::default()
        }));
        // Control-plane pairing already applied: unfiltered replicator on addr1.
        *store.applied.lock().unwrap() = PairingApplied {
            collections: set(&["AgentNetwork"]),
            replicator_addresses: set(&["addr1"]),
            replicator_filter: PairingFilters::new(),
        };
        let admin = MockAdmin {
            active: Mutex::new(vec!["peer-a".into()]),
            fail_connect: true,
            ..Default::default()
        };
        *admin.collections.lock().unwrap() = set(&[&mock_collection_id("AgentNetwork")]);
        admin.replicators.lock().unwrap().insert(
            "addr1".into(),
            RemoteReplicator {
                id: Some("id-addr1".into()),
                collections: vec!["AgentNetwork".into()],
                address: Some("addr1".into()),
            },
        );

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick must reconcile without dialing");

        assert!(
            admin.connects.lock().unwrap().is_empty(),
            "already-active peer must not be redialed"
        );
        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::InstallCollection("AgentRequest".into()),
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
        // Applied records the conversation filter — this is what surfaces as
        // `PeerPairingApplied.replicator_filter` and what the demo waits on.
        assert_eq!(
            store.applied.lock().unwrap().replicator_filter,
            conversation_filter
        );
        let recorded = admin.recorded_filters.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].1, conversation_filter);
    }

    /// Regression for the demo layer-order race: the data-plane desired lands
    /// before the control-plane layer, so the first tick installs the
    /// replicator carrying only the conversation collections. When the merged
    /// desired arrives — same address, same filter, LARGER collection set —
    /// the replicator must be reinstalled with the merged set: the carried
    /// collection set is part of the replicator identity (Lean
    /// `collections_change_forces_reinstall`). Pre-fix the diff keyed
    /// replicators on address alone and converged falsely, so the
    /// control-plane collections were never pushed to the peer (demo `pair`
    /// step-8 hang even with a healthy connection).
    #[tokio::test]
    async fn grown_replicator_collection_set_reinstalls_replicator() {
        let conversation_filter = one_filter("AgentRequest", "agent_did", "did:key:host");
        // Tick 1: only the data-plane layer is visible (Push template shape:
        // nothing subscribed, the filtered replicator carries the set).
        let store = MockStore::with_desired(Some(PairingDesired {
            collections: BTreeSet::new(),
            replicator_addresses: set(&["addr1"]),
            replicator_collections: set(&["AgentRequest"]),
            replicator_filter: conversation_filter.clone(),
            ..Default::default()
        }));
        let admin = MockAdmin {
            active: Mutex::new(vec!["peer-a".into()]),
            ..Default::default()
        };

        let first = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("first tick");
        assert_eq!(
            first.ops_applied,
            vec![DiffOp::InstallReplicator("addr1".into())]
        );

        // The control-plane layer merges in: same address, same filter,
        // larger replicator collection set plus the control subscription.
        *store.desired.lock().unwrap() = Ok(Some(PairingDesired {
            collections: set(&["AgentNetwork"]),
            replicator_addresses: set(&["addr1"]),
            replicator_collections: set(&["AgentNetwork", "AgentRequest"]),
            replicator_filter: conversation_filter,
            ..Default::default()
        }));

        let second = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("second tick");
        assert_eq!(
            second.ops_applied,
            vec![
                DiffOp::InstallCollection("AgentNetwork".into()),
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
        // The reinstalled replicator carries the merged collection set.
        assert_eq!(
            admin.replicators.lock().unwrap()["addr1"].collections,
            vec![
                mock_collection_id("AgentNetwork"),
                mock_collection_id("AgentRequest")
            ]
        );

        // Tick 3: converged — the collections identity must not churn.
        let third = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("third tick");
        assert!(
            third.ops_applied.is_empty(),
            "converged, got: {:?}",
            third.ops_applied
        );
    }

    /// The active-peer gate must fail open: a broken `active_peers` read means
    /// "assume not connected" and dial as before, never a wedged pairing.
    #[tokio::test]
    async fn active_peer_read_failure_still_dials() {
        let store = MockStore::with_desired(Some(PairingDesired {
            collections: set(&["c1"]),
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }));
        let admin = MockAdmin {
            fail_active_peers: true,
            ..Default::default()
        };

        reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr1"]]);
    }

    /// A different peer being active is not this peer being active: the tick
    /// must still dial.
    #[tokio::test]
    async fn other_active_peer_does_not_suppress_dial() {
        let store = MockStore::with_desired(Some(PairingDesired {
            collections: set(&["c1"]),
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }));
        let admin = MockAdmin {
            active: Mutex::new(vec!["peer-b".into()]),
            ..Default::default()
        };

        reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr1"]]);
    }

    /// Review Finding #1: the remote subscription set is tracked in *id*-space by
    /// the adapter (`list_p2p_collections` returns ids), while desired state and
    /// the persisted `PeerPairingApplied` row are in *name*-space. `read_actual`
    /// reverse-resolves the ids to names so the diff compares like with like. A
    /// first tick installs the collection; a SECOND tick must observe convergence
    /// (zero ops). With the pre-fix code the desired name never matched the actual
    /// id, so every sweep re-emitted `InstallCollection` forever.
    ///
    /// The teeth: the mock's `list_p2p_collections` returns a distinct id
    /// (`col_<name>_id`), so convergence only holds because reverse-resolution
    /// maps that id back to the name. If `resolve_collection_name` echoed the id,
    /// actual(id) would never equal desired(name) and this test would fail.
    #[tokio::test]
    async fn second_tick_converges_across_name_and_id_spaces() {
        let store = MockStore::with_desired(Some(PairingDesired {
            collections: set(&["AgentRequest"]),
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }));
        let admin = MockAdmin::default();

        let first = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("first tick");
        assert!(
            first
                .ops_applied
                .iter()
                .any(|op| matches!(op, DiffOp::InstallCollection(_))),
            "first tick installs the collection: {:?}",
            first.ops_applied
        );

        // Applied must persist the collection *name* (the observable contract),
        // not the internal id.
        assert_eq!(
            store.applied.lock().unwrap().collections,
            set(&["AgentRequest"]),
            "Applied persists the collection name"
        );

        let second = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("second tick");
        assert!(
            second.ops_applied.is_empty(),
            "second tick must be a no-op (converged), got: {:?}",
            second.ops_applied
        );
    }

    #[tokio::test]
    async fn teardown_is_restricted_to_applied_actual_extras() {
        // Applied holds collection *names* (the observable contract). The remote
        // subscription set is tracked in id-space internally by the mock, but
        // `read_actual` reverse-resolves it to names for the diff.
        let store = MockStore::with_desired(Some(PairingDesired::default()));
        *store.applied.lock().unwrap() = PairingApplied {
            collections: set(&["managed"]),
            replicator_addresses: set(&["managed-addr"]),
            ..Default::default()
        };
        let admin = MockAdmin::default();
        *admin.collections.lock().unwrap() = set(&[
            &mock_collection_id("managed"),
            &mock_collection_id("manual"),
        ]);
        admin.replicators.lock().unwrap().insert(
            "managed-addr".into(),
            RemoteReplicator {
                id: Some("managed-id".into()),
                collections: vec!["managed".into()],
                address: Some("managed-addr".into()),
            },
        );
        admin.replicators.lock().unwrap().insert(
            "manual-addr".into(),
            RemoteReplicator {
                id: Some("manual-id".into()),
                collections: vec!["manual".into()],
                address: Some("manual-addr".into()),
            },
        );

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::TeardownCollection("managed".into()),
                DiffOp::TeardownReplicator("managed-addr".into())
            ]
        );
        assert_eq!(
            *admin.collections.lock().unwrap(),
            set(&[&mock_collection_id("manual")])
        );
        assert!(admin
            .replicators
            .lock()
            .unwrap()
            .contains_key("manual-addr"));
    }

    #[tokio::test]
    async fn desired_absent_tears_down_managed_state_and_deletes_applied_row() {
        let store = MockStore::with_desired(None);
        *store.applied.lock().unwrap() = PairingApplied {
            collections: set(&["c1"]),
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        };
        let admin = MockAdmin::default();
        *admin.collections.lock().unwrap() = set(&[&mock_collection_id("c1")]);
        admin.replicators.lock().unwrap().insert(
            "addr1".into(),
            RemoteReplicator {
                id: Some("id-addr1".into()),
                collections: vec!["c1".into()],
                address: Some("addr1".into()),
            },
        );

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::TeardownCollection("c1".into()),
                DiffOp::TeardownReplicator("addr1".into())
            ]
        );
        assert_eq!(*store.deleted.lock().unwrap(), 1);
        assert!(store.applied.lock().unwrap().is_empty());
    }

    #[test]
    fn nullable_graphql_arrays_emit_null_when_empty() {
        assert_eq!(graphql_nullable_string_array(&BTreeSet::new()), "null");
        assert_eq!(
            graphql_nullable_string_array(&set(&["a", "b"])),
            r#"["a", "b"]"#
        );
    }

    fn desired_row(template: Option<&str>, agent_did: Option<&str>) -> PairingStateRow {
        PairingStateRow {
            agent_did: agent_did.map(str::to_string),
            collections: None,
            replicator_addresses: Some(vec!["addr1".into()]),
            template: template.map(str::to_string),
            replicator_filter: None,
        }
    }

    /// A `Push` template (conversation) resolves to NO subscription collections
    /// (no gossip leak) and a per-peer scope filter over the template set.
    #[test]
    fn push_template_resolves_to_filter_without_subscription() {
        let desired = desired_from_pairing_row(
            desired_row(Some("conversation"), Some("did:key:bob")),
            "did:key:self",
        )
        .expect("template resolves")
        .expect("some desired layer");

        assert!(
            desired.collections.is_empty(),
            "Push templates must not subscribe"
        );
        assert!(desired.replicator_collections.contains("AgentRequest"));
        let pred = desired
            .replicator_filter
            .get("AgentRequest")
            .expect("AgentRequest filter");
        assert_eq!(pred.field, "agent_did");
        assert_eq!(pred.value, "did:key:bob");
    }

    /// A `Replicate` template (agent-config) subscribes to its collection set
    /// and carries an EMPTY (unfiltered) replicator filter.
    #[test]
    fn replicate_template_resolves_to_subscription_without_filter() {
        let desired = desired_from_pairing_row(
            desired_row(Some("agent-config"), Some("did:key:bob")),
            "did:key:self",
        )
        .expect("template resolves")
        .expect("some desired layer");

        assert!(desired.collections.contains("AgentBehavior"));
        assert_eq!(desired.collections, desired.replicator_collections);
        assert!(
            desired.replicator_filter.is_empty(),
            "Replicate templates are unfiltered"
        );
    }

    /// Rows without a template default to `conversation` (matches the migration
    /// backfill), and an unknown template also falls back to the default.
    #[test]
    fn missing_and_unknown_template_default_to_conversation() {
        let missing =
            desired_from_pairing_row(desired_row(None, Some("did:key:bob")), "did:key:self")
                .expect("default resolves")
                .expect("some desired layer");
        assert!(missing.collections.is_empty());
        assert!(missing.replicator_filter.contains_key("AgentRequest"));

        let unknown = desired_from_pairing_row(
            desired_row(Some("not-a-template"), Some("did:key:bob")),
            "did:key:self",
        )
        .expect("default resolves")
        .expect("some desired layer");
        assert_eq!(
            unknown.replicator_collections,
            missing.replicator_collections
        );
        assert!(unknown.replicator_filter.contains_key("AgentRequest"));
    }

    #[test]
    fn subagent_coordinator_template_filters_parent_and_bridge_directionally() {
        let desired = desired_from_pairing_row(
            desired_row(Some("subagent-coordinator"), Some("did:key:host")),
            "did:key:coord",
        )
        .expect("subagent coordinator template resolves")
        .expect("some desired layer");

        assert!(desired.collections.is_empty());
        assert_eq!(
            desired.replicator_collections,
            set(&["AgentRequest", "AgentToolCall"])
        );
        assert_eq!(
            desired
                .replicator_filter
                .get("AgentRequest")
                .map(|filter| (filter.field.as_str(), filter.value.as_str())),
            Some(("agent_did", "did:key:coord"))
        );
        assert_eq!(
            desired
                .replicator_filter
                .get("AgentToolCall")
                .map(|filter| (filter.field.as_str(), filter.value.as_str())),
            Some(("spawn_target_did", "did:key:host"))
        );
    }

    #[test]
    fn subagent_host_template_filters_conversation_to_local_host() {
        let desired = desired_from_pairing_row(
            desired_row(Some("subagent-host"), Some("did:key:coord")),
            "did:key:host",
        )
        .expect("subagent host template resolves")
        .expect("some desired layer");

        assert!(desired.collections.is_empty());
        assert!(desired.replicator_collections.contains("AgentToolCall"));
        assert_eq!(desired.replicator_filter.len(), 8);
        for predicate in desired.replicator_filter.values() {
            assert_eq!(predicate.field, "agent_did");
            assert_eq!(predicate.value, "did:key:host");
        }
    }

    #[test]
    fn app_collections_on_control_plane_path_soft_skips() {
        // A base/PeerPairingDesired row naming app-collections has no way to
        // supply row collections; it must resolve to no wiring (soft-skip),
        // never an empty-collection replicator.
        let out = desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:peer".to_string()),
                collections: None,
                replicator_addresses: Some(vec!["addr-b".to_string()]),
                template: Some("app-collections".to_string()),
                replicator_filter: None,
            },
            "did:key:self",
        )
        .expect("resolve ok");
        assert!(
            out.is_none(),
            "app-collections is invalid for a control-plane row"
        );
    }

    #[test]
    fn app_collections_row_resolves_row_collections_as_subscription_and_replicator() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:peer-b".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let layer = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:self".to_string()),
                collections: Some(vec!["ChangeProposed".to_string()]),
                replicator_addresses: None,
                template: Some("app-collections".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:self",
        )
        .expect("resolve ok")
        .expect("some layer");
        assert!(layer.replicator_collections.contains("ChangeProposed"));
        assert!(
            layer.collections.contains("ChangeProposed"),
            "app-collections must subscribe (Replicate)"
        );
        assert!(layer.replicator_filter.is_empty(), "unscoped => no filter");
        assert!(layer.template_ids.contains("app-collections"));
    }

    #[test]
    fn app_collections_empty_collections_soft_skips() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:peer-b".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let out = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:self".to_string()),
                collections: Some(vec!["   ".to_string()]),
                replicator_addresses: None,
                template: Some("app-collections".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:self",
        )
        .expect("resolve ok (soft-skip is Ok(None), not Err)");
        assert!(
            out.is_none(),
            "empty/blank app-collections set must soft-skip to None"
        );
    }

    /// Residual (documented, not softened in #657): a foreign `agent_did` on a
    /// data-plane row still hard-fails the whole peer load (`desired_read_failed`),
    /// including a co-existing control pairing. Security refusal, not soft-skip.
    #[test]
    fn foreign_agent_did_still_hard_fails_whole_peer_load() {
        let signed_endpoint = NetworkEndpointEntry {
            peer_id: "peer-b".to_string(),
            agent_did: "did:key:peer-b".to_string(),
            address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        };
        let err = data_plane_desired_from_pairing_row(
            PairingStateRow {
                agent_did: Some("did:key:someone-else".to_string()),
                collections: Some(vec!["ChangeProposed".to_string()]),
                replicator_addresses: None,
                template: Some("app-collections".to_string()),
                replicator_filter: None,
            },
            &signed_endpoint,
            "did:key:self",
        )
        .expect_err("foreign agent_did must hard-fail, not soft-skip");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("foreign") || msg.contains("someone-else") || msg.contains("refusing"),
            "error should name the refusal: {msg}"
        );
    }

    /// End-to-end reconcile of a `Push` (conversation) template: a filtered
    /// replicator is installed and NO subscription (`add_p2p_collections`) is.
    #[tokio::test]
    async fn push_template_installs_filtered_replicator_without_subscription() {
        let store = MockStore::with_desired(Some(
            desired_from_pairing_row(
                desired_row(Some("conversation"), Some("did:key:bob")),
                "did:key:self",
            )
            .expect("template resolves")
            .expect("some desired layer"),
        ));
        let admin = MockAdmin::default();

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        // Only a replicator install; no collection subscription.
        assert_eq!(
            outcome.ops_applied,
            vec![DiffOp::InstallReplicator("addr1".into())]
        );
        let emitted = admin.emitted.lock().unwrap();
        assert!(
            !emitted
                .iter()
                .any(|op| matches!(op, DiffOp::InstallCollection(_))),
            "Push template must NOT subscribe: {emitted:?}"
        );
        drop(emitted);

        // The recorded replicator carries the per-peer scope filter.
        let calls = admin.recorded_filters.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let pred = calls[0]
            .1
            .get("AgentRequest")
            .expect("AgentRequest filter on installed replicator");
        assert_eq!(pred.field, "agent_did");
        assert_eq!(pred.value, "did:key:bob");
    }

    /// End-to-end reconcile of a `Replicate` (agent-config) template: it both
    /// subscribes (`add_p2p_collections`) and installs an UNFILTERED replicator.
    #[tokio::test]
    async fn replicate_template_subscribes_and_replicates() {
        let store = MockStore::with_desired(Some(
            desired_from_pairing_row(
                desired_row(Some("agent-config"), Some("did:key:bob")),
                "did:key:self",
            )
            .expect("template resolves")
            .expect("some desired layer"),
        ));
        let admin = MockAdmin::default();

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        let emitted = admin.emitted.lock().unwrap();
        assert!(
            emitted
                .iter()
                .any(|op| matches!(op, DiffOp::InstallCollection(_))),
            "Replicate template must subscribe: {emitted:?}"
        );
        assert!(emitted
            .iter()
            .any(|op| matches!(op, DiffOp::InstallReplicator(_))));
        drop(emitted);
        assert!(outcome
            .ops_applied
            .iter()
            .any(|op| matches!(op, DiffOp::InstallReplicator(_))));

        // The installed replicator is unfiltered.
        let calls = admin.recorded_filters.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].1.is_empty(),
            "Replicate template must install an unfiltered replicator"
        );
    }

    /// End-to-end: a changed scoped DID (different filter) reinstalls the
    /// replicator — teardown of the old filtered identity, install of the new.
    #[tokio::test]
    async fn changing_scoped_did_reinstalls_replicator() {
        let store = MockStore::with_desired(Some(
            desired_from_pairing_row(
                desired_row(Some("conversation"), Some("did:key:bob")),
                "did:key:self",
            )
            .expect("template resolves")
            .expect("some desired layer"),
        ));
        // Applied state: addr1 already installed under a DIFFERENT (alice) filter.
        let mut alice_filter = PairingFilters::default();
        for col in resolve_template("conversation").unwrap().collections.iter() {
            alice_filter.insert(
                (*col).to_string(),
                crate::agent::p2p_reconcile::templates::FilterPredicate {
                    field: "agent_did".to_string(),
                    value: "did:key:alice".to_string(),
                },
            );
        }
        *store.applied.lock().unwrap() = PairingApplied {
            collections: BTreeSet::new(),
            replicator_addresses: set(&["addr1"]),
            replicator_filter: alice_filter,
        };
        let admin = MockAdmin::default();
        // The remote already has the old replicator on addr1.
        admin.replicators.lock().unwrap().insert(
            "addr1".into(),
            RemoteReplicator {
                id: Some("id-addr1".into()),
                collections: vec!["AgentRequest".into()],
                address: Some("addr1".into()),
            },
        );

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
        // The reinstalled replicator carries the NEW (bob) filter.
        let calls = admin.recorded_filters.lock().unwrap();
        let last = calls.last().expect("an install happened");
        assert_eq!(
            last.1
                .get("AgentRequest")
                .expect("AgentRequest filter")
                .value,
            "did:key:bob"
        );
    }

    // -----------------------------------------------------------------------
    // T2: filters at the RemoteP2pAdmin seam
    // -----------------------------------------------------------------------

    /// Verifies that the `MockAdmin` recording captures `PairingFilters` passed
    /// to `add_replicator`, and that an empty `PairingFilters` records as empty
    /// (back-compat) while a non-empty one is faithfully recorded.
    #[tokio::test]
    async fn add_replicator_records_filters_at_seam() {
        use crate::agent::p2p_reconcile::templates::FilterPredicate;

        let admin = MockAdmin::default();
        let addresses = vec!["addr-a".to_string()];
        let collections: Vec<String> = vec![];

        // Back-compat: empty filters record as empty.
        admin
            .add_replicator(&addresses, &collections, &PairingFilters::default())
            .await
            .expect("add_replicator empty filters");

        let calls = admin.recorded_filters.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].1.is_empty(),
            "empty filters should record as empty"
        );
        drop(calls);

        // Non-empty filters are faithfully recorded.
        let mut filters = PairingFilters::default();
        filters.insert(
            "AgentRequest".to_string(),
            FilterPredicate {
                field: "agent_did".to_string(),
                value: "did:key:alice".to_string(),
            },
        );
        admin
            .add_replicator(&addresses, &collections, &filters)
            .await
            .expect("add_replicator non-empty filters");

        let calls = admin.recorded_filters.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let recorded = &calls[1].1;
        assert_eq!(recorded.len(), 1);
        let pred = recorded.get("AgentRequest").expect("AgentRequest filter");
        assert_eq!(pred.field, "agent_did");
        assert_eq!(pred.value, "did:key:alice");
    }
}
