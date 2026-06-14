//! Conformance fence for `Proofs/PeerRegistryDiscovery/`.
//!
//! Bridges the Lean derivation model to the Rust discovery reconciler. Each test
//! names the Lean theorem it mirrors. The ownership invariant (the whole point
//! of R5) is exercised through the [`DiscoveryStore`] seam with a fake that
//! holds a *separate* operator-owned partition the discovery step must never
//! touch — the Rust analogue of the Lean two-finset partition.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use defra_agent::agent::p2p_reconcile::discovery::{
    decide_join_admission, derive_registry_desired, reconcile_discovery_tick, DiscoveredEntry,
    DiscoveryStore, JoinAdmission, RegistryMemberRow,
};

fn entry(peer: &str, live: bool) -> DiscoveredEntry {
    DiscoveredEntry {
        peer_id: peer.to_string(),
        agent_did: format!("did:key:{peer}"),
        addresses: vec![format!("/ip4/1/tcp/1/p2p/{peer}")],
        templates: vec!["conversation".to_string()],
        live,
    }
}

/// Mirrors Lean `mem_deriveRegistryDesired` + `self_not_mem_derive`: a peer is
/// derived iff it is a live, non-self registry entry.
#[test]
fn derive_matches_live_non_self_membership() {
    let reg = vec![
        entry("self", true),
        entry("peerA", true),
        entry("peerB", false),
    ];
    let d = derive_registry_desired("self", &reg);
    assert_eq!(d, BTreeSet::from(["peerA".to_string()]));
}

/// Mirrors Lean `derive_idempotent` / `derive_convergent`: the derivation is a
/// pure function of the registry, so it is stable across ticks.
#[test]
fn derive_is_idempotent_and_convergent_over_stable_registry() {
    let reg = vec![entry("peerA", true), entry("peerB", true)];
    let first = derive_registry_desired("self", &reg);
    let second = derive_registry_desired("self", &reg);
    assert_eq!(first, second);
    assert_eq!(
        first,
        BTreeSet::from(["peerA".to_string(), "peerB".to_string()])
    );
}

/// Fake store exposing ONLY the registry-owned partition (models the
/// `source = "registry"` GraphQL predicate). The operator-owned set is held
/// separately and the test asserts the discovery step never names it.
struct PartitionStore {
    self_peer: String,
    registry: Vec<DiscoveredEntry>,
    registry_owned: Mutex<BTreeSet<String>>,
    operator_owned: BTreeSet<String>,
    deletes: Mutex<Vec<String>>,
    upserts: Mutex<Vec<String>>,
}

impl PartitionStore {
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
            deletes: Mutex::new(Vec::new()),
            upserts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl DiscoveryStore for PartitionStore {
    async fn self_peer_id(&self) -> Result<String> {
        Ok(self.self_peer.clone())
    }
    async fn load_registry(&self) -> Result<Vec<DiscoveredEntry>> {
        Ok(self.registry.clone())
    }
    async fn list_registry_owned_peers(&self) -> Result<BTreeSet<String>> {
        Ok(self.registry_owned.lock().unwrap().clone())
    }
    async fn list_operator_owned_peers(&self) -> Result<BTreeSet<String>> {
        Ok(self.operator_owned.clone())
    }
    async fn upsert_registry_desired(&self, entry: &DiscoveredEntry) -> Result<()> {
        self.registry_owned
            .lock()
            .unwrap()
            .insert(entry.peer_id.clone());
        self.upserts.lock().unwrap().push(entry.peer_id.clone());
        Ok(())
    }
    async fn delete_registry_desired(&self, peer_id: &str) -> Result<()> {
        self.registry_owned.lock().unwrap().remove(peer_id);
        self.deletes.lock().unwrap().push(peer_id.to_string());
        Ok(())
    }
}

/// Mirrors Lean `ownership_safe`: a discovery tick never mutates an
/// operator-owned row. Here peerA is operator-owned with no registry entry; the
/// tick materializes peerB but leaves peerA entirely alone.
#[tokio::test]
async fn ownership_safe_operator_rows_never_touched() {
    let store = PartitionStore::new("self", vec![entry("peerB", true)], &[], &["peerA"]);

    let outcome = reconcile_discovery_tick(&store).await.expect("tick");

    assert_eq!(outcome.upserted, BTreeSet::from(["peerB".to_string()]));
    assert!(outcome.retracted.is_empty());
    // No operator-owned peer was ever upserted or deleted.
    for op in store
        .upserts
        .lock()
        .unwrap()
        .iter()
        .chain(store.deletes.lock().unwrap().iter())
    {
        assert!(
            !store.operator_owned.contains(op),
            "discovery touched operator-owned row {op}"
        );
    }
}

/// Mirrors Lean `retraction_sound` / `retraction_drops_unique_source` /
/// `retraction_preserves_others`: staling an entry retracts exactly its
/// registry-owned row, and an operator-owned row for a different peer survives.
#[tokio::test]
async fn retraction_sound_removes_only_staled_registry_row() {
    let store = PartitionStore::new(
        "self",
        vec![entry("peerA", false), entry("peerB", true)],
        &["peerA"],
        &["peerB"],
    );

    let outcome = reconcile_discovery_tick(&store).await.expect("tick");

    assert_eq!(outcome.retracted, BTreeSet::from(["peerA".to_string()]));
    assert_eq!(*store.deletes.lock().unwrap(), vec!["peerA".to_string()]);
    // peerB is operator-owned: operator intent wins (the union collapses to the
    // operator row under the single-row-per-peer index), so discovery neither
    // deletes nor duplicates it.
    assert!(!outcome.upserted.contains("peerB"));
    assert!(!store.deletes.lock().unwrap().contains(&"peerB".to_string()));
    assert!(!store.upserts.lock().unwrap().contains(&"peerB".to_string()));
}

// ---------------------------------------------------------------------------
// Signed-invite membership gate (Lean `signedByMember` / `isMember`).
//
// These fence the registry-membership half of the join authorization gate via
// the real `decide_join_admission` engine fn — the same predicate the CLI join
// path calls. Token signature validity (`sigValid`) is checked separately at
// token decode (defra-agent-protocol::pairing_token) and is out of this fence.
// ---------------------------------------------------------------------------

fn member_row(did: &str, status: &str, age: ChronoDuration) -> RegistryMemberRow {
    RegistryMemberRow {
        agent_did: did.to_string(),
        status: status.to_string(),
        updated_at: Some(Utc::now() - age),
    }
}

const STALE_AFTER: Duration = Duration::from_secs(90);
const FRESH: ChronoDuration = ChronoDuration::seconds(10);
const STALE: ChronoDuration = ChronoDuration::seconds(200);

/// Mirrors the TOFU bootstrap arm: an empty registry (or one holding only our
/// own self-registration row) admits any signed invite.
#[test]
fn join_gate_empty_or_self_only_registry_is_tofu_bootstrap() {
    let now = Utc::now();
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &[], now, STALE_AFTER),
        JoinAdmission::TofuBootstrap
    );
    let self_only = [member_row("did:key:self", "online", FRESH)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &self_only, now, STALE_AFTER),
        JoinAdmission::TofuBootstrap
    );
}

/// Mirrors `isMember`: a live (online + fresh) issuer row admits the join.
#[test]
fn join_gate_admits_live_member_issuer() {
    let rows = [member_row("did:key:issuer", "online", FRESH)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &rows, Utc::now(), STALE_AFTER),
        JoinAdmission::MemberAdmitted
    );
}

/// Mirrors `non_member_invite_rejected`: with a non-empty registry, an issuer
/// that is absent, offline, or stale is NOT a live member, so the join is
/// rejected (the TOFU arm does not apply once peers exist).
#[test]
fn join_gate_rejects_non_live_member_when_registry_nonempty() {
    let now = Utc::now();
    // Issuer absent (a different peer is the only member).
    let absent = [member_row("did:key:other", "online", FRESH)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &absent, now, STALE_AFTER),
        JoinAdmission::Rejected
    );
    // Issuer present but offline.
    let offline = [member_row("did:key:issuer", "offline", FRESH)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &offline, now, STALE_AFTER),
        JoinAdmission::Rejected
    );
    // Issuer present and online but heartbeat is stale.
    let stale = [member_row("did:key:issuer", "online", STALE)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &stale, now, STALE_AFTER),
        JoinAdmission::Rejected
    );
}
