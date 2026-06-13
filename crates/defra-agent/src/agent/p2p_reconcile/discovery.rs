//! Service-discovery reconciler: read `PeerRegistry`, materialize
//! **registry-owned** `PeerPairingDesired` rows, and let the (unchanged)
//! pairing reconciler wire them.
//!
//! This sits *above* the proven `PairingReconcile` machine. It is the Rust
//! mirror of the Lean model `Proofs/PeerRegistryDiscovery/`. The binding
//! ownership decision from that model: desired rows are a disjoint partition of
//! **operator-owned** and **registry-owned** rows, and the discovery step only
//! ever writes or deletes registry-owned rows — it never reads, writes, or
//! deletes operator-owned rows. We carry that partition as a `source`
//! discriminator field on `PeerPairingDesired` (`"operator"` | `"registry"`),
//! *queried as a partition* (`filter: { source: { _eq: "registry" } }`).
//!
//! Mirrored Lean properties:
//! - `deriveRegistryDesired(self, registry)` = live, non-self registry entries
//!   → desired peers. Mirrored by [`derive_registry_desired`].
//! - `ownership_safe`: the discovery step never mutates an operator-owned row.
//!   Mirrored by only ever touching `source = "registry"` rows.
//! - `retraction_sound`: removing/staling an entry removes exactly its
//!   registry-owned row. Mirrored by deleting the registry-owned row for any
//!   peer no longer derived.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use tokio_util::sync::CancellationToken;

use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::engine::PAIRING_SWEEP_INTERVAL;
use super::registry::REGISTRY_HEARTBEAT_INTERVAL;

/// The `source` discriminator value for operator-authored desired rows.
pub const SOURCE_OPERATOR: &str = "operator";
/// The `source` discriminator value for registry-derived (discovery-owned)
/// desired rows.
pub const SOURCE_REGISTRY: &str = "registry";

/// A registry entry is considered stale (effectively offline) once its
/// `updated_at` heartbeat is older than this. Set to 3× the heartbeat interval
/// so a single missed heartbeat does not flap a peer out of discovery.
pub const REGISTRY_STALE_AFTER: Duration =
    Duration::from_secs(REGISTRY_HEARTBEAT_INTERVAL.as_secs() * 3);

/// One `PeerRegistry` row as observed by the discovery reader, with liveness
/// already resolved from `status` + `updated_at` age. This is the Rust analogue
/// of the Lean `RegistryEntry` (which folds heartbeat-freshness and `status`
/// into a single `live` bit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEntry {
    /// The libp2p peer id of the registered node.
    pub peer_id: String,
    /// Effective liveness: `status == "online"` AND heartbeat within
    /// [`REGISTRY_STALE_AFTER`].
    pub live: bool,
}

impl DiscoveredEntry {
    /// Resolve effective liveness from a raw registry row's `status` and
    /// `updated_at`, relative to `now`. Mirrors the Lean model's single `live`
    /// bit: the reader computes it; the derivation takes it as given.
    pub fn from_row(
        peer_id: String,
        status: Option<&str>,
        updated_at: Option<&str>,
        now: DateTime<Utc>,
    ) -> Self {
        let status_online = status.map(str::trim) == Some("online");
        let fresh = updated_at
            .and_then(|raw| DateTime::parse_from_rfc3339(raw.trim()).ok())
            .map(|ts| ts.with_timezone(&Utc))
            .map(|ts| {
                now.signed_duration_since(ts)
                    .to_std()
                    // A future timestamp (clock skew) is treated as fresh.
                    .map(|age| age <= REGISTRY_STALE_AFTER)
                    .unwrap_or(true)
            })
            .unwrap_or(false);
        Self {
            peer_id,
            live: status_online && fresh,
        }
    }
}

/// Pure derivation `registry → desiredₘ`, mirroring Lean `deriveRegistryDesired`:
/// the registry-owned desired peer set is exactly the live entries whose peer is
/// not self.
///
/// This is a function of the registry alone, which is what makes convergence
/// immediate (idempotent, stable across ticks for a stable registry).
pub fn derive_registry_desired(self_peer: &str, registry: &[DiscoveredEntry]) -> BTreeSet<String> {
    registry
        .iter()
        .filter(|entry| entry.live && entry.peer_id != self_peer)
        .map(|entry| entry.peer_id.clone())
        .collect()
}

/// Outcome of a discovery tick: which registry-owned desired rows were created
/// and which were retracted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryTickOutcome {
    /// Peers for which a registry-owned desired row was upserted this tick.
    pub upserted: BTreeSet<String>,
    /// Peers whose registry-owned desired row was deleted this tick.
    pub retracted: BTreeSet<String>,
}

/// Store seam for the discovery reconciler. **Every method here operates only on
/// the registry-owned partition** (`source = "registry"`). The discovery step
/// must never read, write, or delete operator-owned rows; that invariant is the
/// whole point (mirrors Lean `ownership_safe`).
#[async_trait]
pub trait DiscoveryStore: Send + Sync {
    /// This node's own peer id (excluded from derivation).
    async fn self_peer_id(&self) -> Result<String>;

    /// Live, liveness-resolved registry entries.
    async fn load_registry(&self) -> Result<Vec<DiscoveredEntry>>;

    /// Peers that currently have a **registry-owned** desired row.
    async fn list_registry_owned_peers(&self) -> Result<BTreeSet<String>>;

    /// Peers that currently have an **operator-owned** desired row. Read-only:
    /// discovery uses this purely to *exclude* peers where operator intent
    /// already exists (operator intent wins), never to mutate them. This is the
    /// single-row realization of the Lean `effectiveDesired = operatorDesired ∪
    /// registryDesired` union: the `peer_id` unique index means at most one row
    /// per peer, so a peer present in the operator partition is not also
    /// materialized in the registry partition.
    async fn list_operator_owned_peers(&self) -> Result<BTreeSet<String>>;

    /// Upsert a **registry-owned** desired row for `peer_id`.
    async fn upsert_registry_desired(&self, peer_id: &str) -> Result<()>;

    /// Delete the **registry-owned** desired row for `peer_id`. Must not touch
    /// an operator-owned row for the same peer.
    async fn delete_registry_desired(&self, peer_id: &str) -> Result<()>;
}

/// Run one discovery tick: derive the registry-owned desired set, upsert rows
/// for newly-derived peers, and retract registry-owned rows whose peer is no
/// longer derived.
///
/// Ownership invariant (mirrors Lean `ownership_safe` / `retraction_sound`):
/// the diff is computed against the **registry-owned** desired set only, so an
/// operator-owned row for the same peer is never created, mutated, or deleted by
/// this step.
pub async fn reconcile_discovery_tick(store: &dyn DiscoveryStore) -> Result<DiscoveryTickOutcome> {
    let self_peer = store.self_peer_id().await.context("read self peer id")?;
    let registry = store.load_registry().await.context("load registry")?;
    let derived = derive_registry_desired(&self_peer, &registry);
    let operator_owned = store
        .list_operator_owned_peers()
        .await
        .context("list operator-owned desired peers")?;
    // Operator intent wins: realize the `operatorDesired ∪ registryDesired`
    // union under the single-row-per-peer unique index by excluding any peer the
    // operator already authored. We never read further into or mutate those
    // rows — exclusion only.
    let desired = derived
        .difference(&operator_owned)
        .cloned()
        .collect::<BTreeSet<String>>();
    let existing = store
        .list_registry_owned_peers()
        .await
        .context("list registry-owned desired peers")?;

    let mut outcome = DiscoveryTickOutcome::default();

    // Materialize: registry-owned rows for newly-derived peers.
    for peer in desired.difference(&existing) {
        store
            .upsert_registry_desired(peer)
            .await
            .with_context(|| format!("upsert registry-owned desired row for {peer}"))?;
        outcome.upserted.insert(peer.clone());
    }

    // Retract: registry-owned rows whose peer is no longer derived. This is the
    // retraction-sound step — only registry-owned rows are deleted; operator
    // rows are never named by this query.
    for peer in existing.difference(&desired) {
        store
            .delete_registry_desired(peer)
            .await
            .with_context(|| format!("delete registry-owned desired row for {peer}"))?;
        outcome.retracted.insert(peer.clone());
    }

    Ok(outcome)
}

/// Environment variable gating the discovery reconciler. Default OFF: when
/// unset (or not a truthy value), the registry still replicates and `p2p network
/// list` can show peers, but no auto-pairing happens.
pub const DISCOVERY_AUTO_PAIR_ENV: &str = "DEFRA_AGENT_DISCOVERY_AUTO_PAIR";

/// Whether `discovery_auto_pair` is enabled. Read from
/// [`DISCOVERY_AUTO_PAIR_ENV`]; truthy = `1`/`true`/`yes`/`on` (case-insensitive).
pub fn discovery_auto_pair_enabled() -> bool {
    std::env::var(DISCOVERY_AUTO_PAIR_ENV)
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Background daemon: run the discovery reconciler sweep, mirroring
/// [`super::engine::run_pairing_reconciler`]. Subscribes `EventName::Update` to
/// react to registry replication, plus a periodic sweep.
///
/// Gated behind `discovery_auto_pair`: when OFF the daemon idles (the registry
/// still replicates; no rows are materialized). Idle also when the embedded node
/// has no P2P transport.
pub async fn run_discovery_reconciler(
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Result<()> {
    let Some(p2p) = node.p2p_arc() else {
        tracing::debug!("discovery reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    };

    if !discovery_auto_pair_enabled() {
        tracing::debug!(
            env = DISCOVERY_AUTO_PAIR_ENV,
            "discovery reconciler idle because discovery_auto_pair is off"
        );
        cancel.cancelled().await;
        return Ok(());
    }

    let self_peer_id = match p2p.local_peer_id().await {
        Ok(peer_id) => peer_id,
        Err(error) => {
            tracing::warn!(error = %error, "discovery reconciler: local_peer_id failed; idling");
            cancel.cancelled().await;
            return Ok(());
        }
    };

    let store = GraphqlDiscoveryStore::new(node.clone(), self_peer_id);
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(PAIRING_SWEEP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_discovery(&store).await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                sweep_discovery(&store).await;
            }
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("discovery reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "discovery reconciler update subscription dropped messages");
                }
                sweep_discovery(&store).await;
            }
        }
    }
}

async fn sweep_discovery(store: &dyn DiscoveryStore) {
    match reconcile_discovery_tick(store).await {
        Ok(outcome) => {
            if !outcome.upserted.is_empty() || !outcome.retracted.is_empty() {
                tracing::info!(
                    upserted = ?outcome.upserted,
                    retracted = ?outcome.retracted,
                    "discovery reconcile materialized registry-owned pairings"
                );
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "discovery reconcile tick failed");
        }
    }
}

/// GraphQL-backed [`DiscoveryStore`] over the embedded node.
#[derive(Clone)]
pub struct GraphqlDiscoveryStore {
    node: Arc<EmbeddedNode>,
    self_peer_id: String,
}

impl GraphqlDiscoveryStore {
    pub fn new(node: Arc<EmbeddedNode>, self_peer_id: String) -> Self {
        Self { node, self_peer_id }
    }

    /// List the peers whose desired row carries the given `source`, queried as a
    /// partition (`filter: { source: { _eq: "<source>" } }`).
    async fn list_peers_by_source(&self, source: &str) -> Result<BTreeSet<String>> {
        let source = escape_graphql_string(source);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ source: {{ _eq: "{source}" }} }}) {{
                    peer_id
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query PeerPairingDesired by source")?;
        Ok(rows::<PeerIdRow>(&response, "PeerPairingDesired")?
            .into_iter()
            .filter_map(|row| {
                let peer_id = row.peer_id.trim().to_string();
                (!peer_id.is_empty()).then_some(peer_id)
            })
            .collect())
    }
}

#[async_trait]
impl DiscoveryStore for GraphqlDiscoveryStore {
    async fn self_peer_id(&self) -> Result<String> {
        Ok(self.self_peer_id.clone())
    }

    async fn load_registry(&self) -> Result<Vec<DiscoveredEntry>> {
        let query = r#"{
            PeerRegistry {
                peer_id
                status
                updated_at
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query PeerRegistry")?;
        let now = Utc::now();
        Ok(rows::<RegistryRow>(&response, "PeerRegistry")?
            .into_iter()
            .filter_map(|row| {
                let peer_id = row.peer_id.trim().to_string();
                if peer_id.is_empty() {
                    return None;
                }
                Some(DiscoveredEntry::from_row(
                    peer_id,
                    row.status.as_deref(),
                    row.updated_at.as_deref(),
                    now,
                ))
            })
            .collect())
    }

    async fn list_registry_owned_peers(&self) -> Result<BTreeSet<String>> {
        self.list_peers_by_source(SOURCE_REGISTRY).await
    }

    async fn list_operator_owned_peers(&self) -> Result<BTreeSet<String>> {
        self.list_peers_by_source(SOURCE_OPERATOR).await
    }

    async fn upsert_registry_desired(&self, peer_id: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mutation = upsert_registry_desired_mutation(peer_id, &now);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "upsert registry-owned PeerPairingDesired")
    }

    async fn delete_registry_desired(&self, peer_id: &str) -> Result<()> {
        let mutation = delete_registry_desired_mutation(peer_id);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete registry-owned PeerPairingDesired")
    }
}

/// Upsert a registry-owned `PeerPairingDesired` row. The `source` field pins it
/// to the registry partition so the discovery step never blends with operator
/// intent. Empty lists are emitted as `null` (never `[]`).
pub fn upsert_registry_desired_mutation(peer_id: &str, now: &str) -> String {
    let peer_id = escape_graphql_string(peer_id);
    let source = escape_graphql_string(SOURCE_REGISTRY);
    let now = escape_graphql_string(now);
    // The discovery reconciler only materializes the peer membership; collections
    // and replicator addresses come from the registry profiles in a later pass.
    // We emit `null` (never `[]`) for the nillable array columns. The filter is
    // on `peer_id` alone (the unique index): the tick only ever upserts peers
    // that have no operator-owned row, so this never collides with operator
    // intent, and the `source` value pins the materialized row to the registry
    // partition.
    format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    source: "{source}",
                    collections: null,
                    replicator_addresses: null,
                    profiles: null,
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    source: "{source}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

/// Delete the registry-owned `PeerPairingDesired` row for `peer_id`. The
/// `source = "registry"` predicate guarantees an operator-owned row for the same
/// peer is never deleted (mirrors Lean `retraction_sound`).
pub fn delete_registry_desired_mutation(peer_id: &str) -> String {
    let peer_id = escape_graphql_string(peer_id);
    let source = escape_graphql_string(SOURCE_REGISTRY);
    format!(
        r#"mutation {{
            delete_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "{source}" }} }}
            ) {{ _docID }}
        }}"#
    )
}

fn ensure_no_errors(response: &QueryResponse, label: &str) -> Result<()> {
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
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

#[derive(Deserialize)]
struct RegistryRow {
    peer_id: String,
    status: Option<String>,
    updated_at: Option<String>,
}

#[derive(Deserialize)]
struct PeerIdRow {
    peer_id: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn entry(peer: &str, live: bool) -> DiscoveredEntry {
        DiscoveredEntry {
            peer_id: peer.to_string(),
            live,
        }
    }

    // ---- pure derivation (mirrors Lean deriveRegistryDesired) ----

    #[test]
    fn derive_skips_self_and_offline() {
        let reg = vec![
            entry("self", true),
            entry("peerA", true),
            entry("peerB", false),
        ];
        let d = derive_registry_desired("self", &reg);
        assert!(d.contains("peerA"));
        assert!(!d.contains("self")); // self excluded
        assert!(!d.contains("peerB")); // offline excluded
    }

    #[test]
    fn derive_is_idempotent_over_stable_registry() {
        let reg = vec![entry("peerA", true), entry("peerB", true)];
        let once = derive_registry_desired("self", &reg);
        let twice = derive_registry_desired("self", &reg);
        assert_eq!(once, twice);
    }

    // ---- liveness resolution ----

    #[test]
    fn liveness_requires_online_status_and_fresh_heartbeat() {
        let now = DateTime::parse_from_rfc3339("2026-06-13T00:01:40Z")
            .unwrap()
            .with_timezone(&Utc);
        // online + fresh (40s ago, under 90s stale window) => live
        let fresh = DiscoveredEntry::from_row(
            "p".into(),
            Some("online"),
            Some("2026-06-13T00:01:00Z"),
            now,
        );
        assert!(fresh.live);
        // online but stale (>90s ago) => not live
        let stale = DiscoveredEntry::from_row(
            "p".into(),
            Some("online"),
            Some("2026-06-13T00:00:00Z"),
            now,
        );
        assert!(!stale.live);
        // offline status, fresh heartbeat => not live
        let offline = DiscoveredEntry::from_row(
            "p".into(),
            Some("offline"),
            Some("2026-06-13T00:01:00Z"),
            now,
        );
        assert!(!offline.live);
        // missing heartbeat => not live
        let no_hb = DiscoveredEntry::from_row("p".into(), Some("online"), None, now);
        assert!(!no_hb.live);
    }

    // ---- mutation shapes ----

    #[test]
    fn upsert_mutation_pins_source_to_registry_and_emits_null_not_empty_lists() {
        let m = upsert_registry_desired_mutation(r#"peer"a"#, "2026-06-13T00:00:00Z");
        assert!(m.contains(r#"peer_id: { _eq: "peer\"a" }"#));
        assert!(m.contains(r#"source: "registry""#));
        assert!(m.contains("collections: null"));
        assert!(m.contains("replicator_addresses: null"));
        assert!(!m.contains("[]"));
    }

    #[test]
    fn delete_mutation_restricts_to_registry_source() {
        let m = delete_registry_desired_mutation("peerA");
        assert!(m.contains(r#"peer_id: { _eq: "peerA" }"#));
        assert!(m.contains(r#"source: { _eq: "registry" }"#));
    }

    // ---- ownership invariant via the store seam (mirrors Lean ownership_safe /
    //      retraction_sound) ----

    /// A fake store that records all mutations and, crucially, holds a separate
    /// operator-owned set the discovery step must never touch. The store's
    /// methods only ever expose / mutate the registry-owned partition — modeling
    /// the `source = "registry"` GraphQL predicate.
    struct FakeStore {
        self_peer: String,
        registry: Vec<DiscoveredEntry>,
        registry_owned: Mutex<BTreeSet<String>>,
        /// Operator-owned rows. If discovery ever calls a mutation that names a
        /// peer in here, the test asserts it was NOT this set being mutated.
        operator_owned: BTreeSet<String>,
        upserts: Mutex<Vec<String>>,
        deletes: Mutex<Vec<String>>,
    }

    impl FakeStore {
        fn new(
            self_peer: &str,
            registry: Vec<DiscoveredEntry>,
            registry_owned: &[&str],
            operator_owned: &[&str],
        ) -> Self {
            Self {
                self_peer: self_peer.to_string(),
                registry,
                registry_owned: Mutex::new(registry_owned.iter().map(|s| s.to_string()).collect()),
                operator_owned: operator_owned.iter().map(|s| s.to_string()).collect(),
                upserts: Mutex::new(Vec::new()),
                deletes: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl DiscoveryStore for FakeStore {
        async fn self_peer_id(&self) -> Result<String> {
            Ok(self.self_peer.clone())
        }
        async fn load_registry(&self) -> Result<Vec<DiscoveredEntry>> {
            Ok(self.registry.clone())
        }
        async fn list_registry_owned_peers(&self) -> Result<BTreeSet<String>> {
            // Models `filter: { source: { _eq: "registry" } }` — operator-owned
            // rows are NOT visible here.
            Ok(self.registry_owned.lock().unwrap().clone())
        }
        async fn list_operator_owned_peers(&self) -> Result<BTreeSet<String>> {
            // Models `filter: { source: { _eq: "operator" } }` — read-only,
            // used for exclusion (operator intent wins).
            Ok(self.operator_owned.clone())
        }
        async fn upsert_registry_desired(&self, peer_id: &str) -> Result<()> {
            self.registry_owned
                .lock()
                .unwrap()
                .insert(peer_id.to_string());
            self.upserts.lock().unwrap().push(peer_id.to_string());
            Ok(())
        }
        async fn delete_registry_desired(&self, peer_id: &str) -> Result<()> {
            self.registry_owned.lock().unwrap().remove(peer_id);
            self.deletes.lock().unwrap().push(peer_id.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn discovery_diff_only_manages_registry_source_rows() {
        // Operator-owned desired row for peerA, and NO registry entry for peerA.
        // peerB has a live registry entry. Discovery must upsert a registry row
        // for peerB and must NOT delete the operator-owned peerA row.
        let store = FakeStore::new(
            "self",
            vec![entry("peerB", true)],
            /* registry_owned */ &[],
            /* operator_owned */ &["peerA"],
        );

        let outcome = reconcile_discovery_tick(&store).await.expect("tick");

        assert_eq!(outcome.upserted, BTreeSet::from(["peerB".to_string()]));
        assert!(outcome.retracted.is_empty());
        // The operator-owned peerA was never deleted.
        assert!(!store
            .deletes
            .lock()
            .unwrap()
            .iter()
            .any(|p| store.operator_owned.contains(p)));
        // ... and never upserted either.
        assert!(!store
            .upserts
            .lock()
            .unwrap()
            .iter()
            .any(|p| store.operator_owned.contains(p)));
    }

    #[tokio::test]
    async fn staling_entry_retracts_only_its_registry_row() {
        // registry-owned row for peerA exists; peerA's entry has gone offline
        // (not live), so it is no longer derived. peerB has a live entry AND an
        // operator-owned row. Discovery must delete the registry-owned peerA row
        // and never touch (or duplicate) the operator-owned peerB row.
        let store = FakeStore::new(
            "self",
            vec![entry("peerA", false), entry("peerB", true)],
            /* registry_owned */ &["peerA"],
            /* operator_owned */ &["peerB"],
        );

        let outcome = reconcile_discovery_tick(&store).await.expect("tick");

        // peerB is live but operator-owned: operator intent wins, so discovery
        // does NOT materialize a registry row for it (single-row union).
        assert!(!outcome.upserted.contains("peerB"));
        // peerA's stale registry-owned row is retracted...
        assert_eq!(outcome.retracted, BTreeSet::from(["peerA".to_string()]));
        // exactly peerA was deleted, and it is not an operator-owned peer.
        assert_eq!(*store.deletes.lock().unwrap(), vec!["peerA".to_string()]);
        assert!(!store.operator_owned.contains("peerA"));
        // The operator-owned peerB was never upserted or deleted.
        assert!(!store.upserts.lock().unwrap().contains(&"peerB".to_string()));
        assert!(!store.deletes.lock().unwrap().contains(&"peerB".to_string()));
    }
}
