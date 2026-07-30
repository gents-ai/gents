use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use gents::agent::p2p_reconcile::discovery::{
    decide_join_admission, derive_registry_desired, reconcile_discovery_tick, DiscoveredEntry,
    DiscoveryStore, JoinAdmission, RegistryMemberRow,
};
use gents::agent::p2p_reconcile::network::{
    decide_v5_admission, derive_network_desired, peer_is_materializable, reconcile_network_tick,
    select_materializable_entries, select_revoked_member_dids, NetworkEndpointEntry, NetworkStore,
    V5AdmissionClaim, V5Rejection,
};
use gents::identity::{AgentIdentity, KeyIdentity};
use gents_protocol::network_token::{EndpointRecord, MembershipRecord, NetworkRecord};

fn entry(peer: &str, live: bool) -> DiscoveredEntry {
    DiscoveredEntry {
        peer_id: peer.to_string(),
        agent_did: format!("did:key:{peer}"),
        addresses: vec![format!("/ip4/1/tcp/1/p2p/{peer}")],
        templates: vec!["conversation".to_string()],
        live,
    }
}

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
    network_owned: BTreeSet<String>,
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
            network_owned: BTreeSet::new(),
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
    async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>> {
        Ok(self.network_owned.clone())
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

#[tokio::test]
async fn ownership_safe_operator_rows_never_touched() {
    let store = PartitionStore::new("self", vec![entry("peerB", true)], &[], &["peerA"]);

    let outcome = reconcile_discovery_tick(&store).await.expect("tick");

    assert_eq!(outcome.upserted, BTreeSet::from(["peerB".to_string()]));
    assert!(outcome.retracted.is_empty());
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
    assert!(!outcome.upserted.contains("peerB"));
    assert!(!store.deletes.lock().unwrap().contains(&"peerB".to_string()));
    assert!(!store.upserts.lock().unwrap().contains(&"peerB".to_string()));
}

fn member_row(did: &str, status: &str, age: ChronoDuration) -> RegistryMemberRow {
    RegistryMemberRow {
        agent_did: did.to_string(),
        status: status.to_string(),
        updated_at: Some(Utc::now() - age),
    }
}

fn network_entry(peer: &str, did: &str) -> NetworkEndpointEntry {
    NetworkEndpointEntry {
        peer_id: peer.to_string(),
        agent_did: did.to_string(),
        address: format!("/ip4/1/tcp/1/p2p/{peer}"),
    }
}

#[test]
fn derive_network_desired_matches_materializable_non_self_entries() {
    let entries = vec![
        network_entry("peer-self", "did:key:self"),
        network_entry("peer-a", "did:key:a"),
        network_entry("", "did:key:blank-peer"),
        network_entry("peer-b", "did:key:b"),
    ];

    let desired = derive_network_desired("did:key:self", &entries);

    assert_eq!(
        desired,
        BTreeSet::from(["peer-a".to_string(), "peer-b".to_string()])
    );
}

struct NetworkPartitionStore {
    self_did: String,
    entries: Vec<NetworkEndpointEntry>,
    network_owned: Mutex<BTreeSet<String>>,
    non_network_owned: BTreeSet<String>,
    deletes: Mutex<Vec<String>>,
    upserts: Mutex<Vec<String>>,
}

impl NetworkPartitionStore {
    fn new(
        self_did: &str,
        entries: Vec<NetworkEndpointEntry>,
        network_owned: &[&str],
        non_network_owned: &[&str],
    ) -> Self {
        Self {
            self_did: self_did.to_string(),
            entries,
            network_owned: Mutex::new(network_owned.iter().map(|s| s.to_string()).collect()),
            non_network_owned: non_network_owned.iter().map(|s| s.to_string()).collect(),
            deletes: Mutex::new(Vec::new()),
            upserts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl NetworkStore for NetworkPartitionStore {
    async fn self_did(&self) -> Result<String> {
        Ok(self.self_did.clone())
    }

    async fn load_materializable_entries(&self) -> Result<Vec<NetworkEndpointEntry>> {
        Ok(self.entries.clone())
    }

    async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>> {
        Ok(self.network_owned.lock().unwrap().clone())
    }

    async fn list_non_network_owned_peers(&self) -> Result<BTreeSet<String>> {
        Ok(self.non_network_owned.clone())
    }

    async fn upsert_network_desired(&self, entry: &NetworkEndpointEntry) -> Result<()> {
        self.network_owned
            .lock()
            .unwrap()
            .insert(entry.peer_id.clone());
        self.upserts.lock().unwrap().push(entry.peer_id.clone());
        Ok(())
    }

    async fn delete_network_desired(&self, peer_id: &str) -> Result<()> {
        self.network_owned.lock().unwrap().remove(peer_id);
        self.deletes.lock().unwrap().push(peer_id.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn network_reconcile_materializes_only_unblocked_network_peers() {
    let store = NetworkPartitionStore::new(
        "did:key:self",
        vec![
            network_entry("peer-self", "did:key:self"),
            network_entry("peer-a", "did:key:a"),
            network_entry("peer-b", "did:key:b"),
        ],
        &["peer-stale"],
        &["peer-a"],
    );

    let outcome = reconcile_network_tick(&store).await.expect("network tick");

    assert_eq!(outcome.upserted, BTreeSet::from(["peer-b".to_string()]));
    assert_eq!(
        outcome.retracted,
        BTreeSet::from(["peer-stale".to_string()])
    );
    assert_eq!(*store.upserts.lock().unwrap(), vec!["peer-b".to_string()]);
    assert_eq!(
        *store.deletes.lock().unwrap(),
        vec!["peer-stale".to_string()]
    );
    assert!(
        !store
            .upserts
            .lock()
            .unwrap()
            .contains(&"peer-a".to_string()),
        "non-network-owned peer-a must block network materialization"
    );
}

const STALE_AFTER: Duration = Duration::from_secs(90);
const FRESH: ChronoDuration = ChronoDuration::seconds(10);
const STALE: ChronoDuration = ChronoDuration::seconds(200);

// ---------------------------------------------------------------------------
// The materialization GATE itself (Lean `admittedMember` / `memberSignedEndpoint`
// / `revoke_drops_member` / `unsigned_membership_not_materialized` /
// `forged_endpoint_not_materializable`).
//
// The `NetworkPartitionStore` above feeds the reconciler entries that have
// *already* passed the gate, so it cannot fence the gate. These tests drive the
// real `select_materializable_entries` — the executable embodiment of the gate,
// factored out of `GraphqlNetworkStore::load_materializable_entries` — with
// genuinely signed records, so a regression that dropped the `status=="active"`
// filter or stubbed a signature check would fail here.
// ---------------------------------------------------------------------------

fn gate_identity(label: &str) -> KeyIdentity {
    let dir = tempfile::tempdir().expect("tempdir for gate key");
    KeyIdentity::load_or_create(dir.path().join(format!("{label}.key")), None)
        .expect("create gate identity")
}

async fn signed_network(admin: &KeyIdentity) -> NetworkRecord {
    let mut rec = NetworkRecord {
        network_id: "net-gate".to_string(),
        admin_did: admin.did().to_string(),
        display_name: "Gate Net".to_string(),
        default_template: "network-control".to_string(),
        created_at: Utc::now().to_rfc3339(),
        sig: Vec::new(),
    };
    rec.sig = admin
        .sign(&rec.signing_payload())
        .await
        .expect("sign network");
    rec
}

async fn signed_membership(
    admin: &KeyIdentity,
    network_id: &str,
    member_did: &str,
    status: &str,
) -> MembershipRecord {
    let mut rec = MembershipRecord {
        network_id: network_id.to_string(),
        member_did: member_did.to_string(),
        status: status.to_string(),
        granted_at: Utc::now().to_rfc3339(),
        revoked_at: String::new(),
        sig: Vec::new(),
    };
    rec.sig = admin
        .sign(&rec.signing_payload())
        .await
        .expect("sign membership");
    rec
}

async fn signed_endpoint(member: &KeyIdentity, age: ChronoDuration) -> EndpointRecord {
    let mut rec = EndpointRecord {
        did: member.did().to_string(),
        node_id: "peer-node-id".to_string(),
        address: "/ip4/1/tcp/1/p2p/peer-node-id".to_string(),
        updated_at: (Utc::now() - age).to_rfc3339(),
        sig: Vec::new(),
    };
    rec.sig = member
        .sign(&rec.signing_payload())
        .await
        .expect("sign endpoint");
    rec
}

#[tokio::test]
async fn gate_admits_active_signed_member_with_fresh_endpoint() {
    let admin = gate_identity("gate-admit-admin");
    let member = gate_identity("gate-admit-member");
    let net = signed_network(&admin).await;
    let mem = signed_membership(&admin, &net.network_id, member.did(), "active").await;
    let ep = signed_endpoint(&member, FRESH).await;

    let out = select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
        .await
        .expect("gate");

    assert_eq!(out.len(), 1, "valid active member must materialize");
    assert_eq!(out[0].agent_did, member.did());
    assert_eq!(out[0].peer_id, "peer-node-id");
}

#[tokio::test]
async fn gate_excludes_revoked_member() {
    let admin = gate_identity("gate-revoke-admin");
    let member = gate_identity("gate-revoke-member");
    let net = signed_network(&admin).await;
    let mem = signed_membership(&admin, &net.network_id, member.did(), "revoked").await;
    let ep = signed_endpoint(&member, FRESH).await;

    let out = select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
        .await
        .expect("gate");

    assert!(
        out.is_empty(),
        "revoked membership must not materialize (Lean revoke_drops_member)"
    );
}

#[tokio::test]
async fn reciprocal_gate_selects_only_verified_revocations() {
    let admin = gate_identity("reciprocal-revoke-admin");
    let valid_member = gate_identity("reciprocal-revoke-valid");
    let forged_member = gate_identity("reciprocal-revoke-forged");
    let active_member = gate_identity("reciprocal-revoke-active");
    let net = signed_network(&admin).await;
    let valid = signed_membership(&admin, &net.network_id, valid_member.did(), "revoked").await;
    let mut forged =
        signed_membership(&admin, &net.network_id, forged_member.did(), "revoked").await;
    forged.sig = vec![0u8; 64];
    let active = signed_membership(&admin, &net.network_id, active_member.did(), "active").await;

    let revoked = select_revoked_member_dids(&admin, &net, &[valid, forged, active])
        .await
        .expect("revocation gate");

    assert_eq!(revoked, BTreeSet::from([valid_member.did().to_string()]));
}

#[tokio::test]
async fn gate_excludes_forged_membership_signature() {
    let admin = gate_identity("gate-forgemem-admin");
    let member = gate_identity("gate-forgemem-member");
    let net = signed_network(&admin).await;
    let mut mem = signed_membership(&admin, &net.network_id, member.did(), "active").await;
    mem.sig = vec![0u8; 64];
    let ep = signed_endpoint(&member, FRESH).await;

    let out = select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
        .await
        .expect("gate");

    assert!(
        out.is_empty(),
        "membership with invalid admin signature must not materialize"
    );
}

#[tokio::test]
async fn gate_excludes_forged_endpoint_signature() {
    let admin = gate_identity("gate-forgeep-admin");
    let member = gate_identity("gate-forgeep-member");
    let net = signed_network(&admin).await;
    let mem = signed_membership(&admin, &net.network_id, member.did(), "active").await;
    let mut ep = signed_endpoint(&member, FRESH).await;
    ep.sig = vec![0u8; 64];

    let out = select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
        .await
        .expect("gate");

    assert!(
        out.is_empty(),
        "endpoint with invalid member signature must not materialize"
    );
}

#[tokio::test]
async fn gate_excludes_stale_endpoint() {
    let admin = gate_identity("gate-stale-admin");
    let member = gate_identity("gate-stale-member");
    let net = signed_network(&admin).await;
    let mem = signed_membership(&admin, &net.network_id, member.did(), "active").await;
    let ep = signed_endpoint(&member, STALE).await;

    let out = select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
        .await
        .expect("gate");

    assert!(out.is_empty(), "stale endpoint must not materialize");
}

#[tokio::test]
async fn gate_returns_empty_for_invalid_network_signature() {
    let admin = gate_identity("gate-forgenet-admin");
    let member = gate_identity("gate-forgenet-member");
    let mut net = signed_network(&admin).await;
    net.sig = vec![0u8; 64];
    let mem = signed_membership(&admin, &net.network_id, member.did(), "active").await;
    let ep = signed_endpoint(&member, FRESH).await;

    let out = select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
        .await
        .expect("gate");

    assert!(
        out.is_empty(),
        "invalid network signature must materialize nothing"
    );
}

fn valid_v5_claim<'a>() -> V5AdmissionClaim<'a> {
    V5AdmissionClaim {
        issuer_did: "did:key:admin",
        joiner_did: "did:key:member",
        network_admin_did: "did:key:admin",
        network_sig_valid: true,
        network_id_consistent: true,
        grant_member_did: "did:key:member",
        grant_status: "active",
        grant_sig_valid: true,
    }
}

#[test]
fn v5_admits_valid_admin_issued_grant() {
    assert_eq!(decide_v5_admission(&valid_v5_claim()), Ok(()));
}

#[test]
fn v5_rejects_non_admin_issuer() {
    let mut c = valid_v5_claim();
    c.issuer_did = "did:key:not-the-admin";
    assert_eq!(decide_v5_admission(&c), Err(V5Rejection::IssuerNotAdmin));
}

#[test]
fn v5_rejects_invalid_network_signature() {
    let mut c = valid_v5_claim();
    c.network_sig_valid = false;
    assert_eq!(
        decide_v5_admission(&c),
        Err(V5Rejection::InvalidNetworkSignature)
    );
}

#[test]
fn v5_rejects_inconsistent_network_id() {
    let mut c = valid_v5_claim();
    c.network_id_consistent = false;
    assert_eq!(
        decide_v5_admission(&c),
        Err(V5Rejection::InconsistentNetworkId)
    );
}

#[test]
fn v5_rejects_revoked_grant() {
    let mut c = valid_v5_claim();
    c.grant_status = "revoked";
    assert_eq!(decide_v5_admission(&c), Err(V5Rejection::GrantNotActive));
}

#[test]
fn v5_rejects_invalid_grant_signature() {
    let mut c = valid_v5_claim();
    c.grant_sig_valid = false;
    assert_eq!(
        decide_v5_admission(&c),
        Err(V5Rejection::InvalidGrantSignature)
    );
}

#[test]
fn v5_rejects_wrong_grantee() {
    let mut c = valid_v5_claim();
    c.grant_member_did = "did:key:someone-else";
    assert_eq!(decide_v5_admission(&c), Err(V5Rejection::WrongGrantee));
}

#[tokio::test]
async fn data_plane_gate_admits_active_member_and_excludes_self() {
    let admin = gate_identity("dp-admit-admin");
    let member = gate_identity("dp-admit-member");
    let net = signed_network(&admin).await;
    let mem = signed_membership(&admin, &net.network_id, member.did(), "active").await;
    let ep = signed_endpoint(&member, FRESH).await;

    let entries =
        select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
            .await
            .expect("gate");

    assert!(
        peer_is_materializable(&entries, "peer-node-id", admin.did()),
        "active member must be a materializable data-plane peer"
    );
    assert!(
        !peer_is_materializable(&entries, "peer-node-id", member.did()),
        "self must be excluded from data-plane materialization"
    );
}

#[tokio::test]
async fn data_plane_gate_retracts_revoked_member() {
    let admin = gate_identity("dp-revoke-admin");
    let member = gate_identity("dp-revoke-member");
    let net = signed_network(&admin).await;
    let mem = signed_membership(&admin, &net.network_id, member.did(), "revoked").await;
    let ep = signed_endpoint(&member, FRESH).await;

    let entries =
        select_materializable_entries(&admin, &net, &[mem], &[ep], Utc::now(), STALE_AFTER)
            .await
            .expect("gate");

    assert!(
        !peer_is_materializable(&entries, "peer-node-id", admin.did()),
        "revoked member's data-plane edge must retract (D11 master gate)"
    );
}

#[test]
fn join_gate_empty_or_self_only_registry_is_tofu_bootstrap() {
    let now = Utc::now();
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &[], now, STALE_AFTER),
        JoinAdmission::TofuBootstrap
    );
    let self_only = [member_row("did:key:self", "online", FRESH)];
    assert_eq!(
        decide_join_admission(
            "did:key:issuer",
            "did:key:self",
            &self_only,
            now,
            STALE_AFTER
        ),
        JoinAdmission::TofuBootstrap
    );
}

#[test]
fn join_gate_admits_live_member_issuer() {
    let rows = [member_row("did:key:issuer", "online", FRESH)];
    assert_eq!(
        decide_join_admission(
            "did:key:issuer",
            "did:key:self",
            &rows,
            Utc::now(),
            STALE_AFTER
        ),
        JoinAdmission::MemberAdmitted
    );
}

#[test]
fn join_gate_rejects_non_live_member_when_registry_nonempty() {
    let now = Utc::now();
    let absent = [member_row("did:key:other", "online", FRESH)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &absent, now, STALE_AFTER),
        JoinAdmission::Rejected
    );
    let offline = [member_row("did:key:issuer", "offline", FRESH)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &offline, now, STALE_AFTER),
        JoinAdmission::Rejected
    );
    let stale = [member_row("did:key:issuer", "online", STALE)];
    assert_eq!(
        decide_join_admission("did:key:issuer", "did:key:self", &stale, now, STALE_AFTER),
        JoinAdmission::Rejected
    );
}
