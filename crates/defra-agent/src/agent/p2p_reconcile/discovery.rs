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

use super::registry::REGISTRY_HEARTBEAT_INTERVAL;
use super::templates::{resolve_template, ScopeTemplate};

/// The scope template discovery prefers when a peer offers it: the everyday
/// filtered-push of the peer's conversation slice. When a peer does not offer
/// `conversation`, discovery falls back to the first offered template that
/// resolves in the catalog.
pub const PREFERRED_DISCOVERY_TEMPLATE: &str = "conversation";

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

/// Whether a heartbeat timestamp `ts` is fresh relative to `now`: within
/// `stale_after` in the past, OR within `stale_after` in the future. A timestamp
/// further than `stale_after` ahead of `now` is treated as NOT fresh — a
/// far-future stamp is more likely a bad clock or a bogus row than a live peer,
/// and must not pin a dead peer alive indefinitely. Small future skew (clocks
/// slightly out of sync) is still tolerated as fresh.
pub fn heartbeat_is_fresh(ts: DateTime<Utc>, now: DateTime<Utc>, stale_after: Duration) -> bool {
    match now.signed_duration_since(ts).to_std() {
        // ts is in the past: fresh iff the age is within the window.
        Ok(age) => age <= stale_after,
        // ts is in the future (clock skew): fresh iff within the window ahead.
        Err(_) => ts
            .signed_duration_since(now)
            .to_std()
            .map(|ahead| ahead <= stale_after)
            .unwrap_or(false),
    }
}

/// One `PeerRegistry` row as seen by the join-admission membership gate.
#[derive(Debug, Clone)]
pub struct RegistryMemberRow {
    pub agent_did: String,
    pub status: String,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Outcome of the signed-invite membership gate — the Rust mirror of the Lean
/// `signedByMember` registry/TOFU arms (`Proofs/PeerRegistryDiscovery`). Token
/// signature validity (`sigValid`) is enforced separately at token decode; this
/// decides only the registry-membership half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinAdmission {
    /// Registry holds no peer members (excluding self): admit under TOFU
    /// bootstrap. Mirrors `signedByMember … tofuBootstrap=true`.
    TofuBootstrap,
    /// The invite issuer is a live registry member: admit. Mirrors
    /// `isMember tok.issuer reg`.
    MemberAdmitted,
    /// Registry is non-empty and the issuer is not a live member: reject.
    /// Mirrors `non_member_invite_rejected` (the TOFU arm does not apply).
    Rejected,
}

/// Decide whether a join invite from `issuer_did` is admissible against the
/// loaded registry rows. Pure mirror of Lean `isMember` / `signedByMember`'s
/// registry arm: a peer is a live member iff its row is `status == "online"` and
/// its heartbeat is within `stale_after`. `self_did`'s own self-registration row
/// never counts (a registry holding only ourselves is still TOFU bootstrap).
pub fn decide_join_admission(
    issuer_did: &str,
    self_did: &str,
    rows: &[RegistryMemberRow],
    now: DateTime<Utc>,
    stale_after: Duration,
) -> JoinAdmission {
    let self_did = self_did.trim();
    let issuer_did = issuer_did.trim();
    let mut any_members = false;
    let mut issuer_is_live_member = false;
    for row in rows {
        let did = row.agent_did.trim();
        if did.is_empty() {
            continue;
        }
        if !self_did.is_empty() && did == self_did {
            continue;
        }
        any_members = true;
        let online = row.status.trim() == "online";
        let fresh = row
            .updated_at
            .map(|ts| heartbeat_is_fresh(ts, now, stale_after))
            .unwrap_or(false);
        if did == issuer_did && online && fresh {
            issuer_is_live_member = true;
        }
    }
    if !any_members {
        JoinAdmission::TofuBootstrap
    } else if issuer_is_live_member {
        JoinAdmission::MemberAdmitted
    } else {
        JoinAdmission::Rejected
    }
}

/// One `PeerRegistry` row as observed by the discovery reader, with liveness
/// already resolved from `status` + `updated_at` age. This is the Rust analogue
/// of the Lean `RegistryEntry` (which folds heartbeat-freshness and `status`
/// into a single `live` bit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEntry {
    /// The libp2p peer id of the registered node.
    pub peer_id: String,
    /// The agent DID (principal identity) running on the registered node. Used
    /// to stamp `agent_did` on the materialized desired row.
    pub agent_did: String,
    /// Shareable multiaddrs (e.g. `/ip4/.../tcp/.../p2p/<peer_id>`) the peer
    /// advertised. These become the desired row's `replicator_addresses` so the
    /// pairing reconciler has somewhere to replicate.
    pub addresses: Vec<String>,
    /// Scope templates the peer offers (the templates it is willing to
    /// replicate). Discovery picks one and stamps it on the materialized
    /// `PeerPairingDesired.template`; the pairing reconciler then resolves the
    /// collection set, scope filter, and delivery mode from it.
    pub templates: Vec<String>,
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
        agent_did: String,
        addresses: Vec<String>,
        templates: Vec<String>,
        status: Option<&str>,
        updated_at: Option<&str>,
        now: DateTime<Utc>,
    ) -> Self {
        let status_online = status.map(str::trim) == Some("online");
        let fresh = updated_at
            .and_then(|raw| DateTime::parse_from_rfc3339(raw.trim()).ok())
            .map(|ts| ts.with_timezone(&Utc))
            .map(|ts| heartbeat_is_fresh(ts, now, super::intervals::stale_after()))
            .unwrap_or(false);
        Self {
            peer_id,
            agent_did,
            addresses,
            templates,
            live: status_online && fresh,
        }
    }

    /// The scope template discovery will stamp on the materialized desired row,
    /// chosen from the peer's offered templates:
    /// - prefer [`PREFERRED_DISCOVERY_TEMPLATE`] (`conversation`) if offered,
    /// - else the first offered template that resolves in the built-in catalog,
    /// - else `None` (the peer offers nothing we can honor — discovery skips it).
    ///
    /// Unknown/unresolvable offered ids are ignored, never stamped.
    pub fn chosen_template(&self) -> Option<&'static ScopeTemplate> {
        let offered: Vec<&str> = self
            .templates
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        if offered.contains(&PREFERRED_DISCOVERY_TEMPLATE) {
            if let Some(t) = resolve_template(PREFERRED_DISCOVERY_TEMPLATE) {
                return Some(t);
            }
        }
        offered.into_iter().find_map(resolve_template)
    }

    /// The collection set for the materialized desired row's `collections`,
    /// derived from the entry's [chosen template](Self::chosen_template).
    ///
    /// The pairing reconciler treats the stamped `template` as authoritative for
    /// the collection set (it re-resolves from the template at reconcile time),
    /// so this only needs to satisfy the non-nullable `collections` column with
    /// a coherent, non-empty set. Returns `None` when the entry offers no
    /// resolvable template — discovery skips materializing such an entry.
    pub fn desired_collections(&self) -> Option<BTreeSet<String>> {
        let template = self.chosen_template()?;
        Some(
            template
                .collections
                .iter()
                .map(|&c| c.to_string())
                .collect(),
        )
    }
}

/// Pure derivation `registry → desiredₘ`, mirroring Lean `deriveRegistryDesired`:
/// the registry-owned desired peer set is exactly the live entries whose peer is
/// not self.
///
/// This is a function of the registry alone, which is what makes convergence
/// immediate (idempotent, stable across ticks for a stable registry). Whether a
/// derived peer is actually *materialized* is a separate, downstream concern:
/// the tick skips materializing a derived peer that offers no resolvable scope
/// template (see [`reconcile_discovery_tick`]). The derivation deliberately
/// stays the pure live∧¬self predicate so it remains a 1:1 mirror of the Lean
/// model.
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

    /// Peers that currently have a **network-owned** desired row (`source =
    /// "network"`, materialized by the membership reconciler). Read-only:
    /// discovery *excludes* these so the registry-discovery partition never
    /// collides with the network-membership mesh on the unique `peer_id` index.
    /// This is the discovery-side mirror of the network reconciler's
    /// `list_non_network_owned_peers` exclusion — the two partitions yield to
    /// each other symmetrically. A peer that is both a network member and a
    /// registry heartbeat publisher (the normal trusted-fleet case) is therefore
    /// owned by exactly one source.
    async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>>;

    /// Upsert a **registry-owned** desired row for `entry`, stamping the offered
    /// scope template (chosen by [`DiscoveredEntry::chosen_template`]) and
    /// populating the template's collection set plus the replicator addresses the
    /// peer advertised, so the pairing reconciler has a concrete scoped pairing
    /// to install.
    async fn upsert_registry_desired(&self, entry: &DiscoveredEntry) -> Result<()>;

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
    // Index the entries by peer id so the upsert can populate the materialized
    // row's collections/addresses from the entry the peer advertised.
    let entry_by_peer: std::collections::BTreeMap<&str, &DiscoveredEntry> = registry
        .iter()
        .map(|entry| (entry.peer_id.as_str(), entry))
        .collect();
    let operator_owned = store
        .list_operator_owned_peers()
        .await
        .context("list operator-owned desired peers")?;
    let network_owned = store
        .list_network_owned_peers()
        .await
        .context("list network-owned desired peers")?;
    // Operator AND network intent win: realize the `operatorDesired ∪
    // networkDesired ∪ registryDesired` union under the single-row-per-peer
    // unique index by excluding any peer another source already authored. We
    // never read further into or mutate those rows — exclusion only. Excluding
    // the network partition is the symmetric counterpart to the network
    // reconciler's `list_non_network_owned_peers`; without it a peer that is
    // both a network member and a registry heartbeat publisher collides on the
    // unique `peer_id` index (the index then rejects one writer per sweep).
    let blocked: BTreeSet<String> = operator_owned.union(&network_owned).cloned().collect();
    let desired = derived
        .difference(&blocked)
        .cloned()
        .collect::<BTreeSet<String>>();
    let existing = store
        .list_registry_owned_peers()
        .await
        .context("list registry-owned desired peers")?;

    let mut outcome = DiscoveryTickOutcome::default();

    // Materialize: registry-owned rows for newly-derived peers, each populated
    // from the registry entry it was derived from. A derived peer that offers no
    // resolvable scope template is skipped (not materialized) — there is no
    // pairing intent the reconciler could honor, so stamping it would produce an
    // inert row. The skip is a no-op, so it is stable across ticks.
    for peer in desired.difference(&existing) {
        let entry = entry_by_peer
            .get(peer.as_str())
            .copied()
            .with_context(|| format!("derived peer {peer} missing from registry entries"))?;
        if entry.chosen_template().is_none() {
            tracing::debug!(
                peer = %peer,
                offered = ?entry.templates,
                "discovery skips peer: no resolvable scope template offered"
            );
            continue;
        }
        store
            .upsert_registry_desired(entry)
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
///
/// TRUST: enabling this makes the node auto-materialize pairings (and thus
/// replication) from `PeerRegistry` rows, which are replicated, self-asserted,
/// and NOT signature-bound to their claimed `agent_did`. It is therefore a
/// trusted-fleet / TOFU switch: turn it on only when every node that can write
/// the replicated registry is trusted (see #490 review H4).
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

    // Auto-pair is active: surface the trust assumption once. The registry rows
    // that will drive pairing/replication are replicated and self-asserted (no
    // per-row signature binding agent_did to a key), so this is a trusted-fleet
    // decision, not cryptographic authorization (see #490 review H4).
    tracing::warn!(
        env = DISCOVERY_AUTO_PAIR_ENV,
        "discovery auto-pair is ENABLED: pairings will be materialized from \
         replicated, self-asserted PeerRegistry rows — only enable this when \
         every node that can write the registry is trusted"
    );

    let store = GraphqlDiscoveryStore::new(node.clone(), self_peer_id);
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
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
                agent_did
                addresses
                templates
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
                    row.agent_did.unwrap_or_default(),
                    row.addresses.unwrap_or_default(),
                    row.templates.unwrap_or_default(),
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

    async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>> {
        self.list_peers_by_source(super::network::SOURCE_NETWORK)
            .await
    }

    async fn upsert_registry_desired(&self, entry: &DiscoveredEntry) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let template = entry.chosen_template().with_context(|| {
            format!(
                "registry peer {} offers no resolvable scope template",
                entry.peer_id
            )
        })?;
        let collections = entry
            .desired_collections()
            .with_context(|| format!("derive collections for registry peer {}", entry.peer_id))?;
        let mutation = upsert_registry_desired_mutation(entry, template.id, &collections, &now);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "upsert registry-owned PeerPairingDesired")
    }

    async fn delete_registry_desired(&self, peer_id: &str) -> Result<()> {
        let mutation = delete_registry_desired_mutation(peer_id);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete registry-owned PeerPairingDesired")
    }
}

/// Upsert a registry-owned `PeerPairingDesired` row, populated from the registry
/// `entry` the peer advertised. The `source` field pins it to the registry
/// partition so the discovery step never blends with operator intent.
///
/// The row carries:
/// - `template`: the scope template the peer offered (chosen by
///   [`DiscoveredEntry::chosen_template`]); the reconciler resolves the
///   collection set, scope filter, and delivery mode from it,
/// - `collections`: the template's collection set (passed in, guaranteed
///   non-empty — see [`DiscoveredEntry::desired_collections`]); satisfies the
///   non-nullable column and matches what the reconciler re-resolves,
/// - `replicator_addresses`: the entry's advertised addresses,
/// - `agent_did`: copied from the entry (the scope-filter value),
///
/// so the pairing reconciler downstream has a concrete, scoped pairing to
/// install (without this, auto-pair produced an inert row).
///
/// Genuinely-empty lists are emitted as `null` (never `[]`, which corrupts the
/// nillable array columns).
///
/// Ownership safety: the match filter is scoped to
/// `peer_id ∧ source = "registry"` (mirroring [`delete_registry_desired_mutation`]
/// and Lean `ownership_safe`), so the `update` branch can only ever name a
/// registry-owned row. The convergence case still holds: a registry-owned row
/// for the peer carries `source = "registry"`, so it matches and is updated in
/// place on subsequent ticks (no duplicate registry rows — `peer_id` is the
/// unique index). When no registry row matches, the upsert CREATES one from the
/// `add` branch.
///
/// TOCTOU is now fail-safe rather than silently corrupting. Within a single
/// tick, [`reconcile_discovery_tick`] reads operator-owned peers first,
/// subtracts them from the derived set, then upserts the remaining
/// registry-derived peers. If an operator writes a desired row for the *same*
/// peer between that read and this upsert, the scoped filter matches no row, so
/// the upsert attempts a CREATE — which the unique index on `peer_id` rejects
/// (the operator row already occupies it). The tick errors loudly and retries;
/// the next tick excludes the now-operator-owned peer. The previous
/// `peer_id`-only filter would instead have flipped the operator row's `source`
/// to `"registry"`, silently reassigning ownership.
pub fn upsert_registry_desired_mutation(
    entry: &DiscoveredEntry,
    template_id: &str,
    collections: &BTreeSet<String>,
    now: &str,
) -> String {
    let peer_id = escape_graphql_string(&entry.peer_id);
    let agent_did = graphql_nullable_string_literal(Some(entry.agent_did.as_str()));
    let source = escape_graphql_string(SOURCE_REGISTRY);
    let template = escape_graphql_string(template_id);
    let collections = graphql_string_list_literal(collections.iter().map(String::as_str));
    let replicator_addresses =
        graphql_string_list_literal(entry.addresses.iter().map(String::as_str));
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "{source}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: {agent_did},
                    source: "{source}",
                    template: "{template}",
                    collections: {collections},
                    replicator_addresses: {replicator_addresses},
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: {agent_did},
                    source: "{source}",
                    template: "{template}",
                    collections: {collections},
                    replicator_addresses: {replicator_addresses},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

/// Render a GraphQL string-list literal, emitting `null` for an empty list
/// (never `[]`, which types as `JsonArray` and corrupts nillable array columns).
fn graphql_string_list_literal<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<String> = values
        .into_iter()
        .map(|v| format!(r#""{}""#, escape_graphql_string(v)))
        .collect();
    if items.is_empty() {
        "null".to_string()
    } else {
        format!("[{}]", items.join(", "))
    }
}

/// Render a nullable GraphQL string literal, emitting `null` for absent/blank.
fn graphql_nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!(r#""{}""#, escape_graphql_string(v)))
        .unwrap_or_else(|| "null".to_string())
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
    agent_did: Option<String>,
    addresses: Option<Vec<String>>,
    templates: Option<Vec<String>>,
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
            agent_did: format!("did:key:{peer}"),
            addresses: vec![format!("/ip4/1/tcp/1/p2p/{peer}")],
            templates: vec!["conversation".to_string()],
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
            "did:key:p".into(),
            vec![],
            vec![],
            Some("online"),
            Some("2026-06-13T00:01:00Z"),
            now,
        );
        assert!(fresh.live);
        // online but stale (>90s ago) => not live
        let stale = DiscoveredEntry::from_row(
            "p".into(),
            "did:key:p".into(),
            vec![],
            vec![],
            Some("online"),
            Some("2026-06-13T00:00:00Z"),
            now,
        );
        assert!(!stale.live);
        // offline status, fresh heartbeat => not live
        let offline = DiscoveredEntry::from_row(
            "p".into(),
            "did:key:p".into(),
            vec![],
            vec![],
            Some("offline"),
            Some("2026-06-13T00:01:00Z"),
            now,
        );
        assert!(!offline.live);
        // missing heartbeat => not live
        let no_hb = DiscoveredEntry::from_row(
            "p".into(),
            "did:key:p".into(),
            vec![],
            vec![],
            Some("online"),
            None,
            now,
        );
        assert!(!no_hb.live);
    }

    // ---- mutation shapes ----

    #[test]
    fn upsert_mutation_pins_source_to_registry_and_escapes_peer() {
        let mut e = entry(r#"peer"a"#, true);
        e.addresses = vec![];
        let template = e.chosen_template().unwrap();
        let collections = e.desired_collections().unwrap();
        let m =
            upsert_registry_desired_mutation(&e, template.id, &collections, "2026-06-13T00:00:00Z");
        assert!(m.contains(r#"peer_id: { _eq: "peer\"a" }"#));
        assert!(m.contains(r#"source: "registry""#));
        // No raw empty-list literal is ever emitted (corrupts nillable cols).
        assert!(!m.contains("[]"));
    }

    #[test]
    fn upsert_mutation_emits_null_for_genuinely_empty_address_list() {
        // An entry with no advertised addresses still gets a non-empty
        // `collections` (from the chosen template), but its empty address list
        // must render as `null`, never `[]`.
        let mut e = entry("peerEmpty", true);
        e.addresses = vec![];
        let template = e.chosen_template().unwrap();
        let collections = e.desired_collections().unwrap();
        let m =
            upsert_registry_desired_mutation(&e, template.id, &collections, "2026-06-13T00:00:00Z");
        assert!(m.contains("replicator_addresses: null"));
        assert!(m.contains(r#"collections: ["#)); // non-empty
        assert!(!m.contains("[]"));
    }

    #[test]
    fn materialized_registry_row_carries_template_collections_and_address() {
        // A registry entry for peerA offering template "conversation" with
        // address "/ip4/1/tcp/1/p2p/peerA" → the registry-owned desired upsert
        // for peerA stamps template="conversation", includes the conversation
        // collection set (contains "AgentRequest"), and replicator_addresses
        // contains the entry's address, NOT null.
        let e = DiscoveredEntry::from_row(
            "peerA".into(),
            "did:key:peerA".into(),
            vec!["/ip4/1/tcp/1/p2p/peerA".into()],
            vec!["conversation".into()],
            Some("online"),
            Some("2026-06-13T00:00:00Z"),
            DateTime::parse_from_rfc3339("2026-06-13T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let template = e.chosen_template().expect("resolves conversation");
        assert_eq!(template.id, "conversation");
        let collections = e.desired_collections().expect("template collections");
        assert!(
            collections.contains("AgentRequest"),
            "conversation must include AgentRequest: {collections:?}"
        );

        let m =
            upsert_registry_desired_mutation(&e, template.id, &collections, "2026-06-13T00:00:00Z");
        // template is stamped.
        assert!(m.contains(r#"template: "conversation""#), "mutation: {m}");
        // collections is a non-null, non-empty list containing AgentRequest.
        assert!(m.contains(r#""AgentRequest""#), "mutation: {m}");
        assert!(!m.contains("collections: null"), "mutation: {m}");
        // replicator_addresses carries the entry's advertised address, not null.
        assert!(m.contains(r#""/ip4/1/tcp/1/p2p/peerA""#), "mutation: {m}");
        assert!(!m.contains("replicator_addresses: null"), "mutation: {m}");
        // agent_did and source are stamped from the entry / partition.
        assert!(m.contains(r#"agent_did: "did:key:peerA""#), "mutation: {m}");
        assert!(m.contains(r#"source: "registry""#), "mutation: {m}");
    }

    // ---- template selection (chosen_template) ----

    #[test]
    fn chosen_template_prefers_conversation_when_offered() {
        let mut e = entry("p", true);
        e.templates = vec!["agent-config".into(), "conversation".into()];
        assert_eq!(e.chosen_template().map(|t| t.id), Some("conversation"));
    }

    #[test]
    fn chosen_template_falls_back_to_first_resolvable() {
        let mut e = entry("p", true);
        e.templates = vec!["nope".into(), "agent-config".into()];
        assert_eq!(e.chosen_template().map(|t| t.id), Some("agent-config"));
    }

    #[test]
    fn chosen_template_is_none_for_no_resolvable_offer() {
        let mut e = entry("p", true);
        e.templates = vec!["nope".into(), "  ".into()];
        assert!(e.chosen_template().is_none());
        assert!(e.desired_collections().is_none());

        let mut empty = entry("p", true);
        empty.templates = vec![];
        assert!(empty.chosen_template().is_none());
    }

    #[test]
    fn delete_mutation_restricts_to_registry_source() {
        let m = delete_registry_desired_mutation("peerA");
        assert!(m.contains(r#"peer_id: { _eq: "peerA" }"#));
        assert!(m.contains(r#"source: { _eq: "registry" }"#));
    }

    /// The upsert's match filter must be scoped to `source = "registry"` (not
    /// `peer_id` alone). With `peer_id` unique, an operator-owned row for the
    /// same peer cannot be named by the update branch, so discovery can never
    /// flip its `source` from `"operator"` to `"registry"` (mirrors the
    /// `delete_*` predicate and Lean `ownership_safe`). When no registry row
    /// matches, the filter still matches nothing → upsert CREATES a registry
    /// row (the create branch carries the full row). Regression for #2/#15.
    #[test]
    fn upsert_mutation_filter_restricts_to_registry_source() {
        let e = entry("peerA", true);
        let template = e.chosen_template().unwrap();
        let collections = e.desired_collections().unwrap();
        let m =
            upsert_registry_desired_mutation(&e, template.id, &collections, "2026-06-13T00:00:00Z");
        // The match filter is scoped to the registry partition, not peer_id
        // alone, so the update branch can never name an operator-owned row.
        assert!(
            m.contains(r#"source: { _eq: "registry" }"#),
            "upsert filter must scope to source=registry: {m}"
        );
        assert!(
            m.contains(r#"peer_id: { _eq: "peerA" }"#),
            "upsert filter still keyed on peer_id: {m}"
        );
        // The create branch (`add`) still carries the registry source so a
        // brand-new peer is materialized when the filter matches nothing.
        assert!(m.contains(r#"source: "registry""#), "mutation: {m}");
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
        /// Network-membership-owned rows (`source = "network"`). Like
        /// `operator_owned`, read-only and used purely for exclusion.
        network_owned: BTreeSet<String>,
        upserts: Mutex<Vec<String>>,
        deletes: Mutex<Vec<String>>,
        /// (peer_id, stamped template id) for each upsert, so tests can assert
        /// the materialized row carries the offered template.
        upsert_templates: Mutex<Vec<(String, String)>>,
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
                network_owned: BTreeSet::new(),
                upserts: Mutex::new(Vec::new()),
                deletes: Mutex::new(Vec::new()),
                upsert_templates: Mutex::new(Vec::new()),
            }
        }

        /// Seed the network-owned partition (peers the membership reconciler
        /// already owns), which discovery must exclude.
        fn with_network_owned(mut self, peers: &[&str]) -> Self {
            self.network_owned = peers.iter().map(|s| s.to_string()).collect();
            self
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
        async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>> {
            // Models `filter: { source: { _eq: "network" } }` — read-only,
            // used for exclusion (network membership owns the peer).
            Ok(self.network_owned.clone())
        }
        async fn upsert_registry_desired(&self, entry: &DiscoveredEntry) -> Result<()> {
            // Mirror the GraphQL store: stamp the chosen offered template on the
            // materialized row (skipping an entry with no resolvable template is
            // the derivation's job — derive_registry_desired never selects one).
            let template = entry
                .chosen_template()
                .expect("derived entry must offer a resolvable template");
            self.registry_owned
                .lock()
                .unwrap()
                .insert(entry.peer_id.clone());
            self.upserts.lock().unwrap().push(entry.peer_id.clone());
            self.upsert_templates
                .lock()
                .unwrap()
                .push((entry.peer_id.clone(), template.id.to_string()));
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

    /// Regression for the cross-source collision on the unique `peer_id` index:
    /// a peer that is BOTH a live registry entry AND already network-membership-
    /// owned must NOT be materialized as a `source = "registry"` row by
    /// discovery, or the upsert would collide with the existing `source =
    /// "network"` row. Discovery must exclude the network partition symmetrically
    /// with how the network reconciler excludes the registry partition.
    #[tokio::test]
    async fn discovery_excludes_network_owned_peers() {
        // peerNet is a live registry entry but is already network-owned; peerReg
        // is a live registry entry owned by no other source.
        let store = FakeStore::new(
            "self",
            vec![entry("peerNet", true), entry("peerReg", true)],
            /* registry_owned */ &[],
            /* operator_owned */ &[],
        )
        .with_network_owned(&["peerNet"]);

        let outcome = reconcile_discovery_tick(&store).await.expect("tick");

        // Only the unowned peer is materialized; the network-owned peer is
        // excluded (no collision on the unique peer_id index).
        assert_eq!(outcome.upserted, BTreeSet::from(["peerReg".to_string()]));
        assert!(
            !store
                .upserts
                .lock()
                .unwrap()
                .contains(&"peerNet".to_string()),
            "network-owned peerNet must not be materialized as a registry row"
        );
        assert!(outcome.retracted.is_empty());
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

    // ---- T5: discovery stamps the offered scope template ----

    #[tokio::test]
    async fn discovery_stamps_offered_template_on_materialized_row() {
        // A live registry entry for peerA offering ["conversation"] →
        // discovery materializes a source="registry" desired row stamped with
        // template="conversation".
        let mut peer_a = entry("peerA", true);
        peer_a.templates = vec!["conversation".into()];
        let store = FakeStore::new(
            "self",
            vec![peer_a],
            /* registry_owned */ &[],
            /* operator_owned */ &[],
        );

        let outcome = reconcile_discovery_tick(&store).await.expect("tick");

        assert_eq!(outcome.upserted, BTreeSet::from(["peerA".to_string()]));
        let stamped = store.upsert_templates.lock().unwrap().clone();
        assert_eq!(
            stamped,
            vec![("peerA".to_string(), "conversation".to_string())],
            "materialized row must be stamped with the offered template"
        );
    }

    #[tokio::test]
    async fn discovery_skips_entry_offering_no_resolvable_template() {
        // peerA is live but offers only an unknown template; peerB offers
        // nothing at all. Neither is derived, so no registry row is materialized.
        let mut peer_a = entry("peerA", true);
        peer_a.templates = vec!["not-a-template".into()];
        let mut peer_b = entry("peerB", true);
        peer_b.templates = vec![];
        let store = FakeStore::new(
            "self",
            vec![peer_a, peer_b],
            /* registry_owned */ &[],
            /* operator_owned */ &[],
        );

        let outcome = reconcile_discovery_tick(&store).await.expect("tick");

        assert!(
            outcome.upserted.is_empty(),
            "unknown/empty offers must not materialize a row: {outcome:?}"
        );
        assert!(store.upserts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn discovery_still_never_touches_operator_rows() {
        // Ownership invariant preserved under the template path: peerA is
        // operator-owned (and offers nothing), peerB is a live registry offer.
        // Discovery materializes peerB only and never names peerA.
        let mut peer_b = entry("peerB", true);
        peer_b.templates = vec!["conversation".into()];
        let store = FakeStore::new(
            "self",
            vec![peer_b],
            /* registry_owned */ &[],
            /* operator_owned */ &["peerA"],
        );

        let outcome = reconcile_discovery_tick(&store).await.expect("tick");

        assert_eq!(outcome.upserted, BTreeSet::from(["peerB".to_string()]));
        // Operator row peerA was neither upserted nor deleted.
        assert!(!store
            .upserts
            .lock()
            .unwrap()
            .iter()
            .any(|p| store.operator_owned.contains(p)));
        assert!(!store
            .deletes
            .lock()
            .unwrap()
            .iter()
            .any(|p| store.operator_owned.contains(p)));
    }
}
