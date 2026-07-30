//! Service-discovery reconciler: read `PeerRegistry`, materialize
//! mirror of the Lean model `Proofs/PeerRegistryDiscovery/`. The binding
//! Mirrored Lean properties:

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use defra_node::{EmbeddedNode, EventName};
use tokio_util::sync::CancellationToken;

use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::graphql_helpers::{
    ensure_no_errors, graphql_nullable_string_literal, graphql_string_list_literal, rows,
};
use super::registry::REGISTRY_HEARTBEAT_INTERVAL;
use super::templates::{resolve_template, ScopeTemplate};

pub const PREFERRED_DISCOVERY_TEMPLATE: &str = "conversation";

pub const SOURCE_OPERATOR: &str = "operator";
pub const SOURCE_MANIFEST_PREFIX: &str = "manifest:";
pub const SOURCE_REGISTRY: &str = "registry";

pub const REGISTRY_STALE_AFTER: Duration =
    Duration::from_secs(REGISTRY_HEARTBEAT_INTERVAL.as_secs() * 3);

/// and must not pin a dead peer alive indefinitely. Small future skew (clocks
pub fn heartbeat_is_fresh(ts: DateTime<Utc>, now: DateTime<Utc>, stale_after: Duration) -> bool {
    match now.signed_duration_since(ts).to_std() {
        Ok(age) => age <= stale_after,
        Err(_) => ts
            .signed_duration_since(now)
            .to_std()
            .map(|ahead| ahead <= stale_after)
            .unwrap_or(false),
    }
}

#[derive(Debug, Clone)]
pub struct RegistryMemberRow {
    pub agent_did: String,
    pub status: String,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Outcome of the signed-invite membership gate — the Rust mirror of the Lean
/// signature validity (`sigValid`) is enforced separately at token decode; this
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinAdmission {
    TofuBootstrap,
    MemberAdmitted,
    Rejected,
}

/// loaded registry rows. Pure mirror of Lean `isMember` / `signedByMember`'s
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEntry {
    pub peer_id: String,
    pub agent_did: String,
    pub addresses: Vec<String>,
    pub templates: Vec<String>,
    pub live: bool,
}

impl DiscoveredEntry {
    /// `updated_at`, relative to `now`. Mirrors the Lean model's single `live`
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

    pub fn chosen_template(&self) -> Option<&'static ScopeTemplate> {
        let offered: Vec<&str> = self
            .templates
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .filter(|t| !t.starts_with("subagent-"))
            .collect();
        if offered.contains(&PREFERRED_DISCOVERY_TEMPLATE) {
            if let Some(t) = resolve_template(PREFERRED_DISCOVERY_TEMPLATE) {
                return Some(t);
            }
        }
        offered.into_iter().find_map(resolve_template)
    }

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
/// immediate (idempotent, stable across ticks for a stable registry). Whether a
/// stays the pure live∧¬self predicate so it remains a 1:1 mirror of the Lean
pub fn derive_registry_desired(self_peer: &str, registry: &[DiscoveredEntry]) -> BTreeSet<String> {
    registry
        .iter()
        .filter(|entry| entry.live && entry.peer_id != self_peer)
        .map(|entry| entry.peer_id.clone())
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryTickOutcome {
    pub upserted: BTreeSet<String>,
    pub retracted: BTreeSet<String>,
}

/// must never read, write, or delete operator-owned rows; that invariant is the
/// whole point (mirrors Lean `ownership_safe`).
#[async_trait]
pub trait DiscoveryStore: Send + Sync {
    async fn self_peer_id(&self) -> Result<String>;

    async fn load_registry(&self) -> Result<Vec<DiscoveredEntry>>;

    async fn list_registry_owned_peers(&self) -> Result<BTreeSet<String>>;

    async fn list_operator_owned_peers(&self) -> Result<BTreeSet<String>>;

    async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>>;

    async fn upsert_registry_desired(&self, entry: &DiscoveredEntry) -> Result<()>;

    /// Delete the **registry-owned** desired row for `peer_id`. Must not touch
    async fn delete_registry_desired(&self, peer_id: &str) -> Result<()>;
}

/// Ownership invariant (mirrors Lean `ownership_safe` / `retraction_sound`):
pub async fn reconcile_discovery_tick(store: &dyn DiscoveryStore) -> Result<DiscoveryTickOutcome> {
    let self_peer = store.self_peer_id().await.context("read self peer id")?;
    let registry = store.load_registry().await.context("load registry")?;
    let derived = derive_registry_desired(&self_peer, &registry);
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

    for peer in existing.difference(&desired) {
        store
            .delete_registry_desired(peer)
            .await
            .with_context(|| format!("delete registry-owned desired row for {peer}"))?;
        outcome.retracted.insert(peer.clone());
    }

    Ok(outcome)
}

/// and NOT signature-bound to their claimed `agent_did`. It is therefore a
pub const DISCOVERY_AUTO_PAIR_ENV: &str = "GENTS_DISCOVERY_AUTO_PAIR";

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

#[derive(Clone)]
pub struct GraphqlDiscoveryStore {
    node: Arc<EmbeddedNode>,
    self_peer_id: String,
}

impl GraphqlDiscoveryStore {
    pub fn new(node: Arc<EmbeddedNode>, self_peer_id: String) -> Self {
        Self { node, self_peer_id }
    }

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
        let query = r#"{
            PeerPairingDesired {
                peer_id
                source
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query operator-owned PeerPairingDesired rows")?;
        Ok(rows::<PeerSourceRow>(&response, "PeerPairingDesired")?
            .into_iter()
            .filter(|row| source_is_operator_owned(row.source.as_deref()))
            .filter_map(|row| {
                let peer_id = row.peer_id.trim().to_string();
                (!peer_id.is_empty()).then_some(peer_id)
            })
            .collect())
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

fn source_is_operator_owned(source: Option<&str>) -> bool {
    let source = source.map(str::trim).unwrap_or(SOURCE_OPERATOR);
    source == SOURCE_OPERATOR || source.starts_with(SOURCE_MANIFEST_PREFIX)
}

/// Ownership safety: the match filter is scoped to
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

#[derive(Deserialize)]
struct PeerSourceRow {
    peer_id: String,
    source: Option<String>,
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
    fn manifest_provenance_is_an_operator_owned_subpartition() {
        assert!(source_is_operator_owned(None));
        assert!(source_is_operator_owned(Some(SOURCE_OPERATOR)));
        assert!(source_is_operator_owned(Some(
            "manifest:did:key:local-owner"
        )));
        assert!(!source_is_operator_owned(Some(SOURCE_REGISTRY)));
        assert!(!source_is_operator_owned(Some(
            super::super::network::SOURCE_NETWORK
        )));
    }

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
    fn chosen_template_skips_directional_subagent_roles() {
        let mut e = entry("p", true);
        e.templates = vec!["subagent-host".into(), "agent-config".into()];
        assert_eq!(e.chosen_template().map(|t| t.id), Some("agent-config"));

        let mut only_subagent = entry("p", true);
        only_subagent.templates = vec!["subagent-coordinator".into(), "subagent-host".into()];
        assert!(only_subagent.chosen_template().is_none());
        assert!(only_subagent.desired_collections().is_none());
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
