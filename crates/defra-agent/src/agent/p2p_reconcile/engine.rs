//! Runtime pairing reconcile engine.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::network::{GraphqlNetworkStore, NetworkStore};
use super::templates::{resolve_template, scope_filter, Delivery, PairingFilters, Scope};
use super::{
    compute_owned_pairing_diff, DiffOp, EmbeddedRemoteP2pAdmin, PairingActual, PairingApplied,
    PairingDesired, RemoteP2pAdmin,
};

pub const PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTickOutcome {
    pub peer_id: String,
    pub ops_applied: Vec<DiffOp>,
    pub desired_read_failed: bool,
}

#[async_trait]
pub trait PairingStateStore: Send + Sync {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>>;

    async fn load_applied(&self, peer_id: &str) -> Result<PairingApplied>;

    async fn save_applied(&self, peer_id: &str, applied: &PairingApplied) -> Result<()>;

    async fn delete_applied(&self, peer_id: &str) -> Result<()>;

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>>;
}

pub async fn reconcile_peer_tick(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: &str,
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
                desired_read_failed: true,
            });
        }
    };
    let desired_state = desired.clone().unwrap_or_default();
    if desired_state.has_wiring() && !desired_state.replicator_addresses.is_empty() {
        let addresses = desired_state
            .replicator_addresses
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        admin
            .connect(&addresses)
            .await
            .context("connect pairing peer")?;
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
        desired_read_failed: false,
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

    sweep_pairings(&admin, &store).await?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                sweep_pairings_logged(&admin, &store).await;
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
                sweep_pairings_logged(&admin, &store).await;
            }
        }
    }
}

/// Run a sweep, logging (not propagating) a transient failure. A failed sweep —
/// e.g. a momentary `list_peer_ids` read error — must not tear down the whole
/// reconciler; the next tick retries. Mirrors the discovery / heartbeat daemons,
/// which also log-and-continue rather than aborting the runtime task.
async fn sweep_pairings_logged(admin: &dyn RemoteP2pAdmin, store: &dyn PairingStateStore) {
    if let Err(error) = sweep_pairings(admin, store).await {
        tracing::warn!(error = %error, "pairing reconciler sweep failed; retrying on next tick");
    }
}

async fn sweep_pairings(admin: &dyn RemoteP2pAdmin, store: &dyn PairingStateStore) -> Result<()> {
    for peer_id in store.list_peer_ids().await? {
        match reconcile_peer_tick(admin, store, &peer_id).await {
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
            }
            Err(error) => {
                tracing::warn!(
                    peer_id = %peer_id,
                    error = %error,
                    "pairing reconcile tick failed"
                );
            }
        }
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

    Ok(ActualSnapshot {
        state: PairingActual {
            collections,
            replicator_addresses,
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
            // subscription collections.
            let collections = if desired.replicator_collections.is_empty() {
                desired.collections.iter().cloned().collect::<Vec<_>>()
            } else {
                desired
                    .replicator_collections
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            admin
                .add_replicator(&addresses, &collections, &desired.replicator_filter)
                .await
                .with_context(|| format!("install P2P replicator {address}"))
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
}

impl GraphqlPairingStateStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self { node, identity }
    }

    async fn data_plane_peer_is_materializable(&self, peer_id: &str) -> Result<bool> {
        let network = GraphqlNetworkStore::new(self.node.clone(), self.identity.clone());
        Ok(network
            .load_materializable_entries()
            .await?
            .into_iter()
            .any(|entry| entry.peer_id == peer_id && entry.agent_did != self.identity.did()))
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
                    replicator_addresses
                    template
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query pairing desired state")?;
        let base = first_row::<PairingStateRow>(&response, "PeerPairingDesired")?
            .map(desired_from_pairing_row)
            .transpose()?;
        let data_plane = if self
            .data_plane_peer_is_materializable(&raw_peer_id)
            .await
            .with_context(|| format!("checking network membership gate for {raw_peer_id}"))?
        {
            first_row::<PairingStateRow>(&response, "DataPlanePairingDesired")?
                .map(desired_from_pairing_row)
                .transpose()?
        } else {
            None
        };
        Ok(merge_desired(base, data_plane))
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

fn desired_from_pairing_row(row: PairingStateRow) -> Result<PairingDesired> {
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

    // The scope filter value is the peer's agent DID. A peer-DID-scoped template
    // with a blank agent_did cannot be honored: it would build an `agent_did == ""`
    // predicate (matches nothing) or, worse, an unscoped replicator. Refuse the
    // row and skip this peer (caught per-peer by the sweep), mirroring the
    // discovery-side skip of blank-DID registry entries.
    let peer_did = row.agent_did.as_deref().map(str::trim).unwrap_or_default();
    if peer_did.is_empty() && matches!(template.scope, Scope::PeerDid { .. }) {
        anyhow::bail!(
            "pairing row for peer-DID-scoped template {template_id:?} has a blank \
             agent_did; refusing to install an unscoped replicator (skipping peer)"
        );
    }
    let replicator_collections = template
        .collections
        .iter()
        .map(|&c| c.to_string())
        .collect::<BTreeSet<_>>();
    let replicator_filter = scope_filter(&template.scope, template.collections, peer_did);

    let subscription_collections = match template.delivery {
        // Push: never subscribe — the filtered replicator is the only channel,
        // so the unfiltered collection never gossips.
        Delivery::Push => BTreeSet::new(),
        // Replicate: subscribe to the whole collection set.
        Delivery::Replicate => replicator_collections.clone(),
    };

    Ok(PairingDesired {
        collections: subscription_collections,
        replicator_addresses,
        replicator_collections,
        replicator_filter,
    })
}

fn merge_desired(
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    match (base, data_plane) {
        (None, None) => None,
        (Some(desired), None) | (None, Some(desired)) => Some(desired),
        (Some(mut left), Some(right)) => {
            left.collections.extend(right.collections);
            left.replicator_addresses.extend(right.replicator_addresses);
            left.replicator_collections
                .extend(right.replicator_collections);
            left.replicator_filter.extend(right.replicator_filter);
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
    use crate::agent::p2p_reconcile::{RemoteP2pAdminResult, RemoteReplicator};

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
        };
        let data = PairingDesired {
            collections: BTreeSet::new(),
            replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
            replicator_collections: set(&["AgentRequest"]),
            replicator_filter: one_filter("AgentRequest", "agent_did", "did:key:a"),
        };

        let merged = merge_desired(Some(control), Some(data)).expect("merged desired");
        assert_eq!(
            merged.replicator_collections,
            set(&["AgentNetwork", "NetworkMembership", "AgentRequest"])
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
    }

    #[async_trait]
    impl RemoteP2pAdmin for MockAdmin {
        async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
            self.connects.lock().unwrap().push(addresses.to_vec());
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
                self.replicators.lock().unwrap().insert(
                    address.clone(),
                    RemoteReplicator {
                        id: Some(format!("id-{address}")),
                        collections: collections.to_vec(),
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
        let desired =
            desired_from_pairing_row(desired_row(Some("conversation"), Some("did:key:bob")))
                .expect("template resolves");

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
        let desired =
            desired_from_pairing_row(desired_row(Some("agent-config"), Some("did:key:bob")))
                .expect("template resolves");

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
        let missing = desired_from_pairing_row(desired_row(None, Some("did:key:bob")))
            .expect("default resolves");
        assert!(missing.collections.is_empty());
        assert!(missing.replicator_filter.contains_key("AgentRequest"));

        let unknown =
            desired_from_pairing_row(desired_row(Some("not-a-template"), Some("did:key:bob")))
                .expect("default resolves");
        assert_eq!(
            unknown.replicator_collections,
            missing.replicator_collections
        );
        assert!(unknown.replicator_filter.contains_key("AgentRequest"));
    }

    /// End-to-end reconcile of a `Push` (conversation) template: a filtered
    /// replicator is installed and NO subscription (`add_p2p_collections`) is.
    #[tokio::test]
    async fn push_template_installs_filtered_replicator_without_subscription() {
        let store = MockStore::with_desired(Some(
            desired_from_pairing_row(desired_row(Some("conversation"), Some("did:key:bob")))
                .expect("template resolves"),
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
            desired_from_pairing_row(desired_row(Some("agent-config"), Some("did:key:bob")))
                .expect("template resolves"),
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
            desired_from_pairing_row(desired_row(Some("conversation"), Some("did:key:bob")))
                .expect("template resolves"),
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
