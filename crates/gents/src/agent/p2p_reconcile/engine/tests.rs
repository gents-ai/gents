use std::sync::Mutex;

use anyhow::anyhow;
use events::Bus;

use super::*;
use crate::agent::p2p_reconcile::{
    single_string_eq, to_replication_filters, RemoteP2pAdminError, RemoteP2pAdminResult,
    RemoteReplicator,
};

mod desired_state;
use desired_state::desired_row;

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

const TEST_TRANSPORT_PEER_ID: &str =
    "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
const TEST_TRANSPORT_ADDRESS_A: &str =
    "127.0.0.1:56091/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
const TEST_TRANSPORT_ADDRESS_B: &str =
    "127.0.0.1:56092/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";

fn one_filter(collection: &str, field: &str, value: &str) -> PairingFilters {
    let mut filters = PairingFilters::new();
    filters.insert(
        collection.to_string(),
        crate::agent::p2p_reconcile::equality_filter(field, value),
    );
    filters
}

fn merge_desired(
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    merge_layered_desired("did:key:local", "did:key:peer", base, data_plane)
}

#[test]
fn self_pairing_row_is_not_materialized() {
    let base = desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:self".to_string()),
            collections: None,
            replicator_addresses: Some(vec!["iroh-ticket".to_string()]),
            template: Some("machine".to_string()),
            ..Default::default()
        },
        "did:key:self",
    )
    .expect("pairing row parses");

    assert!(merge_layered_desired("did:key:self", "did:key:self", base, None).is_none());
}

#[test]
fn explicit_client_route_keeps_requester_and_owner_across_remote_admin() {
    let desired = desired_from_pairing_row(
        PairingStateRow {
            peer_id: Some("directory-a:runtime-to-client".to_string()),
            agent_did: Some("did:key:mandrake".to_string()),
            replicator_addresses: Some(vec!["iroh-ticket".to_string()]),
            template: Some("client".to_string()),
            ..Default::default()
        },
        // Desktop authenticates the remote-admin request as the phone. The
        // owner must still come from durable intent, not this actor DID.
        "did:key:phone",
    )
    .expect("explicit route parses")
    .expect("route materializes");
    let encoded = serde_json::to_string(
        desired
            .replicator_filter
            .get("AgentRequest")
            .expect("request filter"),
    )
    .unwrap();
    assert!(encoded.contains("did:key:phone"));
    assert!(encoded.contains("did:key:mandrake"));
}

#[test]
fn merge_desired_unions_control_and_data_plane_state() {
    let control = PairingDesired {
        collections: set(&["ControlA", "ControlB"]),
        replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
        replicator_collections: set(&["ControlA", "ControlB"]),
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

    let merged = merge_desired(Some(control), Some(data)).expect("merged desired");
    assert_eq!(
        merged.replicator_collections,
        set(&["ControlA", "ControlB", "AgentRequest"])
    );
    assert_eq!(
        merged.collections,
        set(&["ControlA", "ControlB"]),
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
            .and_then(single_string_eq),
        Some(("agent_did", "did:key:a"))
    );
    assert!(!merged.replicator_filter.contains_key("ControlA"));
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

    let merged = merge_desired(None, Some(data)).expect("data-plane desired");
    assert!(
        merged.collections.is_empty(),
        "data-plane-only desired must not subscribe to conversation collections"
    );
    assert_eq!(merged.replicator_collections, set(&["AgentRequest"]));
    assert!(merged.replicator_filter.contains_key("AgentRequest"));
}

#[test]
fn data_plane_gate_accepts_current_enrollment_endpoint() {
    let enrollment_entries = vec![EnrollmentEndpointEntry {
        peer_id: "peer-network".to_string(),
        agent_did: "did:key:network".to_string(),
        address: "/ticket/network".to_string(),
        desired_id: "peer-network".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    }];

    let entry = materialized_enrollment_entry(&enrollment_entries, "peer-network", "did:key:self")
        .expect("enrollment endpoint should pass gate");

    assert_eq!(entry.address, "/ticket/network");
}

#[tokio::test(start_paused = true)]
async fn cached_data_plane_generation_expires_without_waiting_for_a_sweep() {
    let entry = EnrollmentEndpointEntry {
        peer_id: "peer-network".to_string(),
        agent_did: "did:key:network".to_string(),
        address: "/ticket/network".to_string(),
        desired_id: "peer-network".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2026-08-30T12:00:01Z".to_string(),
    };
    let before = "2026-08-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let expired = "2026-08-30T12:00:01Z".parse::<DateTime<Utc>>().unwrap();

    assert!(enrollment_entry_is_fresh_at(&entry, before));
    assert!(!enrollment_entry_is_fresh_at(&entry, expired));
}

#[test]
fn enrollment_base_and_local_data_plane_have_disjoint_owners() {
    let enrollment_entries = vec![EnrollmentEndpointEntry {
        peer_id: "peer-network".to_string(),
        agent_did: "did:key:network".to_string(),
        address: "/ticket/network".to_string(),
        desired_id: "peer-network".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    }];
    let entry = materialized_enrollment_entry(&enrollment_entries, "peer-network", "did:key:self")
        .expect("enrollment endpoint should pass gate");
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            enrollment_request_digest: Some("digest".to_string()),
            enrollment_authorization_sequence: Some(1),
            enrollment_authorization_expires_at: Some("2099-09-29T00:00:00Z".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_some());
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("operator".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            enrollment_request_digest: Some("digest".to_string()),
            enrollment_authorization_sequence: Some(2),
            enrollment_authorization_expires_at: Some("2099-09-29T00:00:00Z".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());
    assert!(local_data_plane_row(
        PairingStateRow {
            source: Some("operator".to_string()),
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_some());
    assert!(local_data_plane_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());

    let canonical = enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            enrollment_request_digest: Some("digest".to_string()),
            enrollment_authorization_sequence: Some(1),
            enrollment_authorization_expires_at: Some("2099-09-29T00:00:00Z".to_string()),
            agent_did: Some("did:key:attacker".to_string()),
            template: Some("app-collections".to_string()),
            collections: Some(vec!["HostileCollection".to_string()]),
            replicator_addresses: Some(vec!["/ticket/attacker".to_string()]),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .expect("the enrollment owner matches");
    assert_eq!(canonical.agent_did.as_deref(), Some("did:key:self"));
    assert_eq!(canonical.template.as_deref(), Some("client"));
    assert_eq!(canonical.collections, None);
    assert_eq!(
        canonical.replicator_addresses,
        Some(vec!["/ticket/network".to_string()])
    );
}

#[test]
fn data_plane_gate_rejects_self_endpoint_from_both_sources() {
    let enrollment_entries = vec![EnrollmentEndpointEntry {
        peer_id: "peer-self".to_string(),
        agent_did: "did:key:self".to_string(),
        address: "/ticket/self".to_string(),
        desired_id: "peer-self".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    }];

    let entry = materialized_enrollment_entry(&enrollment_entries, "peer-self", "did:key:self");

    assert!(entry.is_none());
}

#[test]
fn data_plane_desired_uses_signed_endpoint_address_and_requester_did() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: None,
            collections: None,
            replicator_addresses: Some(vec!["/ip4/192.0.2.1/tcp/9999/p2p/forged".to_string()]),
            template: Some("conversation".to_string()),
            ..Default::default()
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
            .and_then(single_string_eq),
        Some(("requester_did", "did:key:peer-b"))
    );
}

#[test]
fn enrolled_client_outbound_route_preserves_directional_authority() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "runtime-peer".to_string(),
        agent_did: "did:key:runtime".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/runtime-peer".to_string(),
        desired_id: "runtime-peer:client-to-runtime".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            peer_id: Some("runtime-peer:client-to-runtime".to_string()),
            agent_did: Some("did:key:phone".to_string()),
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("client".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:phone",
    )
    .expect("client data-plane desired")
    .expect("some client data-plane layer");

    assert!(desired.replicator_collections.contains("AgentRequest"));
    assert!(!desired.replicator_collections.contains("AgentBehavior"));
    let encoded = serde_json::to_string(
        desired
            .replicator_filter
            .get("AgentRequest")
            .expect("request filter"),
    )
    .unwrap();
    assert!(encoded.contains("requester_did"));
    assert!(encoded.contains("did:key:phone"));
    assert!(encoded.contains("agent_did"));
    assert!(encoded.contains("did:key:runtime"));
}

#[test]
fn enrollment_owned_base_route_has_explicit_runtime_to_client_direction() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "phone-peer".to_string(),
        agent_did: "did:key:phone".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/phone-peer".to_string(),
        desired_id: "phone-peer".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            peer_id: Some("phone-peer".to_string()),
            agent_did: Some("did:key:runtime".to_string()),
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("client".to_string()),
            source: Some("enrollment".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:runtime",
    )
    .expect("enrollment-owned client route")
    .expect("some enrollment base layer");

    assert!(desired.replicator_collections.contains("AgentBehavior"));
    let encoded = serde_json::to_string(
        desired
            .replicator_filter
            .get("AgentRequest")
            .expect("request filter"),
    )
    .unwrap();
    assert!(encoded.contains("requester_did"));
    assert!(encoded.contains("did:key:phone"));
    assert!(encoded.contains("agent_did"));
    assert!(encoded.contains("did:key:runtime"));
}

#[test]
fn protocol_collection_in_app_data_plane_is_rejected_without_stalling_control_pairing() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-app".to_string(),
        agent_did: "did:key:app".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-app".to_string(),
        desired_id: "peer-app".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let data_plane = data_plane_desired_from_pairing_row(
        PairingStateRow {
            collections: Some(vec![
                "ChangeProposed".to_string(),
                "AgentRequest".to_string(),
            ]),
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect("invalid app layer is a soft rejection");
    assert!(data_plane.is_none());

    let control = PairingDesired {
        replicator_addresses: set(&[signed_endpoint.address.as_str()]),
        replicator_collections: set(&["AgentNetwork"]),
        template_ids: set(&["enrollment-base"]),
        ..Default::default()
    };
    assert_eq!(
        merge_desired(Some(control.clone()), data_plane),
        Some(control),
        "a rejected app layer must not stall or weaken the control route"
    );
}

#[test]
fn data_plane_subagent_coordinator_uses_signed_peer_for_targeted_bridge() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:host".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:coord".to_string()),
            collections: None,
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("subagent-coordinator".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:coord",
    )
    .expect("data-plane coordinator desired")
    .expect("some data-plane layer");

    assert!(!desired.replicator_filter.contains_key("AgentRequest"));
    assert_eq!(desired.replicator_collections, set(&["AgentToolCall"]));
    assert_eq!(
        desired
            .replicator_filter
            .get("AgentToolCall")
            .and_then(single_string_eq),
        Some(("spawn_target_did", "did:key:host"))
    );
}

#[test]
fn data_plane_subagent_host_scopes_return_projection_to_signed_requester() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-a".to_string(),
        agent_did: "did:key:coord".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-a".to_string(),
        desired_id: "peer-a".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:host".to_string()),
            collections: None,
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("subagent-host".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:host",
    )
    .expect("data-plane host desired")
    .expect("some data-plane layer");

    assert_eq!(
        desired.replicator_collections,
        set(&[
            "AgentRequest",
            "AgentResponse",
            "AgentMessage",
            "AgentToolCall"
        ])
    );
    assert_eq!(desired.replicator_filter.len(), 4);
    for predicate in desired.replicator_filter.values() {
        assert_eq!(
            single_string_eq(predicate),
            Some(("requester_did", "did:key:coord"))
        );
    }
}

#[test]
fn data_plane_desired_rejects_foreign_agent_did_scope() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let error = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:someone-else".to_string()),
            collections: None,
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("conversation".to_string()),
            ..Default::default()
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

fn mock_live_filters(filters: &PairingFilters) -> defra_p2p_adapter::ReplicationFilters {
    to_replication_filters(filters)
        .expect("mock filters are representable")
        .into_iter()
        .map(|(collection, filter)| (mock_collection_id(&collection), filter))
        .collect()
}

struct MockStore {
    desired: Mutex<Result<Option<PairingDesired>, String>>,
    applied: Mutex<PairingApplied>,
    saved: Mutex<Vec<PairingApplied>>,
    deleted: Mutex<usize>,
    list_peer_ids_failures: Mutex<usize>,
    list_peer_ids_calls: Mutex<usize>,
    list_peer_ids_retry_started: Option<Arc<tokio::sync::Notify>>,
    list_peer_ids_retry_release: Option<Arc<tokio::sync::Notify>>,
    persist_applied_completed: Option<Arc<tokio::sync::Notify>>,
}

impl Default for MockStore {
    fn default() -> Self {
        Self {
            desired: Mutex::new(Ok(None)),
            applied: Mutex::new(PairingApplied::default()),
            saved: Mutex::new(Vec::new()),
            deleted: Mutex::new(0),
            list_peer_ids_failures: Mutex::new(0),
            list_peer_ids_calls: Mutex::new(0),
            list_peer_ids_retry_started: None,
            list_peer_ids_retry_release: None,
            persist_applied_completed: None,
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

    async fn load_applied(&self, _peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self.applied.lock().unwrap().clone(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, _peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        *self.applied.lock().unwrap() = applied.state.clone();
        if applied.state.is_empty() {
            *self.deleted.lock().unwrap() += 1;
        } else {
            self.saved.lock().unwrap().push(applied.state.clone());
            if let Some(completed) = &self.persist_applied_completed {
                completed.notify_one();
            }
        }
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        *self.list_peer_ids_calls.lock().unwrap() += 1;
        let should_fail = {
            let mut failures = self.list_peer_ids_failures.lock().unwrap();
            if *failures == 0 {
                false
            } else {
                *failures -= 1;
                true
            }
        };
        if should_fail {
            anyhow::bail!("transient list_peer_ids failure");
        }
        if let Some(started) = &self.list_peer_ids_retry_started {
            started.notify_one();
        }
        if let Some(release) = &self.list_peer_ids_retry_release {
            release.notified().await;
        }
        Ok(set(&["peer-a"]))
    }
}

struct MultiPeerStore {
    desired: BTreeMap<String, PairingDesired>,
    applied: Mutex<BTreeMap<String, PairingApplied>>,
}

struct EnrollmentFenceStore {
    desired: PairingDesired,
    generation: EnrollmentRouteGeneration,
    applied: Mutex<PairingApplied>,
    authority_checks: Mutex<std::collections::VecDeque<Result<bool, String>>>,
}

#[async_trait]
impl PairingStateStore for EnrollmentFenceStore {
    async fn load_desired(&self, _peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(Some(self.desired.clone()))
    }

    async fn load_desired_with_authority(&self, _peer_id: &str) -> Result<LoadedPairingDesired> {
        Ok(LoadedPairingDesired {
            state: Some(self.desired.clone()),
            enrollment_generation: Some(self.generation.clone()),
        })
    }

    async fn enrollment_generation_is_current(
        &self,
        _generation: &EnrollmentRouteGeneration,
    ) -> Result<bool> {
        self.authority_checks
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(false))
            .map_err(anyhow::Error::msg)
    }

    async fn load_applied(&self, _peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self.applied.lock().unwrap().clone(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, _peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        *self.applied.lock().unwrap() = applied.state.clone();
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        Ok(set(&["peer-a"]))
    }
}

#[async_trait]
impl PairingStateStore for MultiPeerStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(self.desired.get(peer_id).cloned())
    }

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self
                .applied
                .lock()
                .unwrap()
                .get(peer_id)
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        let mut states = self.applied.lock().unwrap();
        if applied.state.is_empty() {
            states.remove(peer_id);
        } else {
            states.insert(peer_id.to_string(), applied.state.clone());
        }
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        Ok(self.desired.keys().cloned().collect())
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
    deleted_replicator_collections: Mutex<Vec<Vec<String>>>,
    /// Entries returned by `active_peers` (bare peer ids or dial addresses,
    /// like the real adapters).
    active: Mutex<Vec<String>>,
    /// When set, `active_peers` fails, exercising the degraded-read path.
    fail_active_peers: bool,
    /// Test-only barrier proving supervisor cancellation can drop an
    /// in-flight admin wait instead of waiting for its per-RPC timeout.
    active_peers_started: Option<Arc<tokio::sync::Notify>>,
    active_peers_release: Option<Arc<tokio::sync::Notify>>,
    /// When set, `connect` fails after recording the call — modeling the
    /// Linux redial-timeout that motivated the active-peer gate.
    fail_connect: bool,
    /// Optional address-specific barrier used to prove that one stale
    /// peer's dial does not head-of-line block a ready peer's sweep.
    blocked_connect_address: Option<String>,
    blocked_connect_started: Option<Arc<tokio::sync::Notify>>,
    blocked_connect_release: Option<Arc<tokio::sync::Notify>>,
    replicator_installed: Option<Arc<tokio::sync::Notify>>,
    /// Number of upcoming replicator installs to fail. This models the
    /// torn reconnect-replay window where delete succeeds but reinstall
    /// transiently fails; the next topology diff must heal it.
    fail_add_replicator_attempts: Mutex<usize>,
}

#[async_trait]
impl RemoteP2pAdmin for MockAdmin {
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
        if let Some(started) = &self.active_peers_started {
            started.notify_one();
        }
        if let Some(release) = &self.active_peers_release {
            release.notified().await;
        }
        if self.fail_active_peers {
            return Err(RemoteP2pAdminError::RpcError("active_peers down".into()));
        }
        Ok(self.active.lock().unwrap().clone())
    }

    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
        self.connects.lock().unwrap().push(addresses.to_vec());
        if self
            .blocked_connect_address
            .as_ref()
            .is_some_and(|blocked| addresses.iter().any(|address| address == blocked))
        {
            if let Some(started) = &self.blocked_connect_started {
                started.notify_one();
            }
            if let Some(release) = &self.blocked_connect_release {
                release.notified().await;
            }
        }
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
        let mut remaining_failures = self.fail_add_replicator_attempts.lock().unwrap();
        if *remaining_failures > 0 {
            *remaining_failures -= 1;
            return Err(RemoteP2pAdminError::RpcError(
                "transient add_replicator failure".into(),
            ));
        }
        drop(remaining_failures);
        let resolved_filters = mock_live_filters(filters);
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
                    filters: Some(resolved_filters.clone()),
                },
            );
            self.emitted
                .lock()
                .unwrap()
                .push(DiffOp::InstallReplicator(address.clone()));
            if let Some(installed) = &self.replicator_installed {
                installed.notify_one();
            }
        }
        Ok(())
    }

    async fn delete_replicator(
        &self,
        id: &str,
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        self.deleted_replicator_collections
            .lock()
            .unwrap()
            .push(collections.to_vec());
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
async fn owned_teardown_removes_endpoint_despite_collection_drift() {
    let admin = MockAdmin::default();
    let owned_address =
        "127.0.0.1:56091/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb"
            .to_string();
    let unrelated_address =
        "127.0.0.1:56092/p2p/7fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bc"
            .to_string();
    admin.replicators.lock().unwrap().insert(
        owned_address.clone(),
        RemoteReplicator {
            id: Some("drifted-owned-replicator".into()),
            collections: vec![mock_collection_id("unexpected-drifted-collection")],
            address: Some(owned_address.clone()),
            filters: Some(Default::default()),
        },
    );
    admin.replicators.lock().unwrap().insert(
        unrelated_address.clone(),
        RemoteReplicator {
            id: Some("unrelated-replicator".into()),
            collections: vec![mock_collection_id("unrelated-collection")],
            address: Some(unrelated_address.clone()),
            filters: Some(Default::default()),
        },
    );

    let removed = teardown_owned_replicators_at_endpoint(&admin, &owned_address)
        .await
        .expect("owned endpoint cleanup");

    assert_eq!(removed, 1);
    let remaining = admin.replicators.lock().unwrap();
    assert!(!remaining.contains_key(&owned_address));
    assert!(remaining.contains_key(&unrelated_address));
    assert_eq!(
        *admin.deleted_replicator_collections.lock().unwrap(),
        vec![vec![mock_collection_id("unexpected-drifted-collection")]],
        "authoritative teardown must pass the live drifted CollectionIDs"
    );
}

#[tokio::test]
async fn cancellation_preempts_in_flight_pairing_sweep_admin_wait() {
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        ..Default::default()
    }));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let admin = MockAdmin {
        active_peers_started: Some(started.clone()),
        active_peers_release: Some(release),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let mut replay_connections = BTreeMap::new();
    let mut failing_peers = BTreeSet::new();
    let sweep = sweep_pairings_logged_until_cancelled(
        &admin,
        &store,
        &mut replay_connections,
        &mut failing_peers,
        &cancel,
    );
    tokio::pin!(sweep);

    tokio::select! {
        _ = started.notified() => {}
        result = &mut sweep => panic!("sweep returned before admin barrier: {result:?}"),
    }

    cancel.cancel();
    let completed = tokio::time::timeout(Duration::from_millis(100), &mut sweep)
        .await
        .expect("cancellation must preempt the in-flight admin wait");
    assert!(!completed, "cancelled sweep must skip its remaining peers");
}

fn enrolled_live_route_store(
    authority_checks: impl IntoIterator<Item = Result<bool, String>>,
) -> EnrollmentFenceStore {
    EnrollmentFenceStore {
        desired: PairingDesired {
            replicator_addresses: set(&["addr1"]),
            replicator_collections: set(&["AgentRequest"]),
            ..Default::default()
        },
        generation: EnrollmentRouteGeneration {
            member_did: "did:key:member".into(),
            member_peer: "peer-a".into(),
            member_ticket: "addr1".into(),
            request_digest: "digest-1".into(),
            authorization_sequence: 1,
            authorization_expires_at: "2099-09-29T00:00:00Z".into(),
        },
        applied: Mutex::new(PairingApplied {
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }),
        authority_checks: Mutex::new(authority_checks.into_iter().collect()),
    }
}

fn admin_with_live_enrolled_route() -> MockAdmin {
    let admin = MockAdmin::default();
    admin.active.lock().unwrap().push("peer-a".into());
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("route-1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );
    admin
}

#[tokio::test]
async fn committed_revocation_between_desired_load_and_apply_tears_down_stale_route() {
    // First check observes the generation loaded by the sweep; the second
    // models a committed revoke before the enrollment subscription refresh.
    let store = enrolled_live_route_store([Ok(true), Ok(false)]);
    let admin = admin_with_live_enrolled_route();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("revoked route reconciles fail closed");

    assert_eq!(
        outcome.ops_applied,
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );
    assert!(admin.connects.lock().unwrap().is_empty());
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert!(store.applied.lock().unwrap().is_empty());
}

#[tokio::test]
async fn enrollment_projection_read_failure_tears_down_instead_of_preserving_live_route() {
    let store = enrolled_live_route_store([Err("projection unavailable".into())]);
    let admin = admin_with_live_enrolled_route();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("unknown enrollment authority closes the route");

    assert!(!outcome.desired_read_failed);
    assert_eq!(
        outcome.ops_applied,
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );
    assert!(admin.connects.lock().unwrap().is_empty());
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert!(store.applied.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn pairing_reconciler_retries_initial_enumeration_failure_then_cancels_cleanly() {
    let retry_started = Arc::new(tokio::sync::Notify::new());
    let retry_release = Arc::new(tokio::sync::Notify::new());
    let convergence_completed = Arc::new(tokio::sync::Notify::new());
    let store = MockStore {
        desired: Mutex::new(Ok(Some(PairingDesired {
            collections: set(&["AgentRequest"]),
            ..Default::default()
        }))),
        list_peer_ids_failures: Mutex::new(1),
        list_peer_ids_retry_started: Some(retry_started.clone()),
        list_peer_ids_retry_release: Some(retry_release.clone()),
        persist_applied_completed: Some(convergence_completed.clone()),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    let event_bus = events::ChannelBus::new();
    let subscription = event_bus.subscribe(&[EventName::Update]);
    let cancel = CancellationToken::new();
    let reconciler = run_pairing_reconciler_loop(&admin, &store, subscription, &cancel);
    tokio::pin!(reconciler);
    let retry_fence_time = tokio::time::Instant::now();

    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = retry_started.notified() => {}
            result = &mut reconciler => {
                panic!("initial enumeration failure terminated reconciler before retry: {result:?}")
            }
        }
    })
    .await
    .expect("immediate first interval tick must start the retry");
    assert_eq!(
        tokio::time::Instant::now(),
        retry_fence_time,
        "startup retry must consume the already-ready first tick without advancing time"
    );
    assert_eq!(
        *store.list_peer_ids_calls.lock().unwrap(),
        2,
        "the interval's immediately-ready first tick must start the retry"
    );
    assert!(
        admin.emitted.lock().unwrap().is_empty(),
        "the failed initial sweep and gated retry must emit no operation"
    );

    let converged = convergence_completed.notified();
    tokio::pin!(converged);
    retry_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = &mut converged => {}
            result = &mut reconciler => {
                panic!("reconciler terminated before retry convergence: {result:?}")
            }
        }
    })
    .await
    .expect("healthy immediate-tick retry must converge");
    assert_eq!(*store.list_peer_ids_calls.lock().unwrap(), 2);
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![DiffOp::InstallCollection("AgentRequest".to_string())]
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_millis(100), &mut reconciler)
        .await
        .expect("cancellation must stop the reconciler");
}

#[tokio::test]
async fn stale_peer_dial_does_not_head_of_line_block_ready_peer() {
    let desired_for = |address: &str| PairingDesired {
        replicator_addresses: set(&[address]),
        replicator_collections: set(&["AgentRequest"]),
        template_ids: set(&["conversation"]),
        ..Default::default()
    };
    let store = MultiPeerStore {
        desired: BTreeMap::from([
            ("peer-a-stale".into(), desired_for("stale-addr")),
            ("peer-z-ready".into(), desired_for("ready-addr")),
        ]),
        applied: Mutex::new(BTreeMap::new()),
    };
    let stale_started = Arc::new(tokio::sync::Notify::new());
    let stale_release = Arc::new(tokio::sync::Notify::new());
    let replicator_installed = Arc::new(tokio::sync::Notify::new());
    let admin = MockAdmin {
        blocked_connect_address: Some("stale-addr".into()),
        blocked_connect_started: Some(stale_started.clone()),
        blocked_connect_release: Some(stale_release.clone()),
        replicator_installed: Some(replicator_installed.clone()),
        ..Default::default()
    };
    let mut replay_connections = BTreeMap::new();
    let mut failing_peers = BTreeSet::new();
    let sweep = sweep_pairings(&admin, &store, &mut replay_connections, &mut failing_peers);
    tokio::pin!(sweep);

    tokio::select! {
        _ = stale_started.notified() => {}
        result = &mut sweep => panic!("sweep returned before stale dial barrier: {result:?}"),
    }
    tokio::time::timeout(Duration::from_millis(500), replicator_installed.notified())
        .await
        .expect("ready peer must install while stale peer dial remains blocked");
    assert!(
        admin.replicators.lock().unwrap().contains_key("ready-addr"),
        "ready peer topology must converge before stale dial is released"
    );

    stale_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), &mut sweep)
        .await
        .expect("sweep must finish after stale dial is released")
        .expect("sweep result");
    assert!(admin.replicators.lock().unwrap().contains_key("stale-addr"));
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
async fn degraded_first_sweep_preserves_startup_replay_without_repeats() {
    let filter = one_filter("AgentRequest", "agent_did", "did:key:local-owner");
    let desired = PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["subagent-host"]),
        ..Default::default()
    };
    let store = MockStore {
        desired: Mutex::new(Err("transient desired read".into())),
        applied: Mutex::new(PairingApplied {
            replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
            replicator_filter: filter,
            ..Default::default()
        }),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![TEST_TRANSPORT_PEER_ID.into()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        TEST_TRANSPORT_ADDRESS_A.into(),
        RemoteReplicator {
            id: Some("id-current-endpoint".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some(TEST_TRANSPORT_ADDRESS_A.into()),
            filters: Some(Default::default()),
        },
    );
    let mut replay_connections = BTreeMap::new();

    // A degraded first sweep must keep the startup replay pending. The
    // startup replay compensates for reconnect edges missed while this
    // daemon was down; recording the peer as already-seen-active here would
    // silently discharge that obligation without performing it.
    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("degraded desired-read sweep");
    assert_eq!(replay_connections.get("peer-a"), Some(&false));

    *store.desired.lock().unwrap() = Ok(Some(desired));
    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("healthy follow-up sweep");

    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
        ],
        "the deferred startup replay must fire on the first healthy sweep"
    );
    assert_eq!(replay_connections.get("peer-a"), Some(&true));

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("steady-state sweep");
    assert_eq!(
        admin.emitted.lock().unwrap().len(),
        2,
        "a steady-state sweep without a connection edge must not replay again"
    );
}

#[tokio::test]
async fn desired_read_failure_during_reconnect_keeps_replay_pending() {
    let filter = one_filter("AgentRequest", "agent_did", "did:key:local-owner");
    let desired = PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["subagent-host"]),
        ..Default::default()
    };
    let store = MockStore {
        desired: Mutex::new(Err("transient desired read".into())),
        applied: Mutex::new(PairingApplied {
            replicator_addresses: set(&["addr1"]),
            replicator_filter: filter,
            ..Default::default()
        }),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-a".into()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );
    let mut replay_connections = BTreeMap::from([("peer-a".to_string(), false)]);

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("degraded reconnect sweep");
    assert_eq!(replay_connections.get("peer-a"), Some(&false));

    *store.desired.lock().unwrap() = Ok(Some(desired));
    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("healthy follow-up replays pending reconnect");

    assert_eq!(replay_connections.get("peer-a"), Some(&true));
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ]
    );
}

#[tokio::test]
async fn failed_reconnect_replay_is_healed_by_next_tick_diff() {
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
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-a".into()]),
        fail_add_replicator_attempts: Mutex::new(1),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );
    let mut replay_connections = BTreeMap::new();

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("sweep contains per-peer replay failure");
    assert_eq!(replay_connections.get("peer-a"), Some(&false));
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("next sweep heals torn replay");
    assert_eq!(replay_connections.get("peer-a"), Some(&true));
    assert!(admin.replicators.lock().unwrap().contains_key("addr1"));
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ]
    );
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
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(mock_live_filters(&filter)),
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
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["subagent-host"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    admin
        .active
        .lock()
        .unwrap()
        .push(TEST_TRANSPORT_PEER_ID.into());
    admin.replicators.lock().unwrap().insert(
        TEST_TRANSPORT_ADDRESS_A.into(),
        RemoteReplicator {
            id: Some("id-current-endpoint".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some(TEST_TRANSPORT_ADDRESS_A.into()),
            filters: Some(mock_live_filters(&filter)),
        },
    );

    let outcome = reconcile_peer_tick_with_replay(&admin, &store, "peer-a", true)
        .await
        .expect("inbound reconnect replay tick");

    assert!(admin.connects.lock().unwrap().is_empty());
    assert_eq!(outcome.replayed_replicators, vec![TEST_TRANSPORT_ADDRESS_A]);
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
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
    let conversation_filter = one_filter("AgentRequest", "requester_did", "did:key:requester");
    // Desired now includes the conversation data plane: same address, new
    // collection, and a scoped filter (identity change ⇒ reinstall).
    let store = MockStore::with_desired(Some(PairingDesired {
        collections: set(&["AgentNetwork", "AgentRequest"]),
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_filter: conversation_filter.clone(),
        ..Default::default()
    }));
    // Control-plane pairing already applied: unfiltered replicator on addr1.
    *store.applied.lock().unwrap() = PairingApplied {
        collections: set(&["AgentNetwork"]),
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_filter: PairingFilters::new(),
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![TEST_TRANSPORT_PEER_ID.into()]),
        fail_connect: true,
        ..Default::default()
    };
    *admin.collections.lock().unwrap() = set(&[&mock_collection_id("AgentNetwork")]);
    admin.replicators.lock().unwrap().insert(
        TEST_TRANSPORT_ADDRESS_A.into(),
        RemoteReplicator {
            id: Some("id-current-endpoint".into()),
            collections: vec!["AgentNetwork".into()],
            address: Some(TEST_TRANSPORT_ADDRESS_A.into()),
            filters: Some(Default::default()),
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
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
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

/// Route rotation is peer-scoped even with other replicators present or an
/// address-less restored record.
#[tokio::test]
async fn changed_endpoint_replaces_same_peer_replicator_teardown_first() {
    let old_address = "stable-peer@127.0.0.1:4100";
    let fresh_address = "stable-peer@127.0.0.1:4200";
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[fresh_address]),
        replicator_collections: set(&["AgentRequest"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[old_address]),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-a".into()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        old_address.into(),
        RemoteReplicator {
            id: Some("stable-peer".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: None,
            filters: Some(Default::default()),
        },
    );
    admin.replicators.lock().unwrap().insert(
        "other-peer@127.0.0.1:4300".into(),
        RemoteReplicator {
            id: Some("other-peer".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("other-peer@127.0.0.1:4300".into()),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("changed endpoint reconcile");

    assert_eq!(
        *admin.connects.lock().unwrap(),
        vec![vec![fresh_address.to_string()]]
    );
    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownReplicator(old_address.to_string()),
            DiffOp::InstallReplicator(fresh_address.to_string()),
        ]
    );
    assert_eq!(
        store.applied.lock().unwrap().replicator_addresses,
        set(&[fresh_address])
    );
}

/// A restored transport route is not authoritative merely because durable
/// `PeerPairingApplied` still matches desired. The live route may carry an old
/// ticket and have lost its filter during restore; both are route-identity
/// drift and must be repaired before readiness is earned.
#[tokio::test]
async fn matching_applied_repairs_restored_unfiltered_stale_address() {
    let peer_id = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
    let stale_address = format!("127.0.0.1:56091/p2p/{peer_id}");
    let desired_address = format!("127.0.0.1:56092/p2p/{peer_id}");
    let filter = one_filter("AgentRequest", "agent_did", "did:key:mandrake");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[&desired_address]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[&desired_address]),
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![peer_id.to_string()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        stale_address.clone(),
        RemoteReplicator {
            id: Some(peer_id.to_string()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some(stale_address),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("repair restored route");

    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownReplicator(desired_address.clone()),
            DiffOp::InstallReplicator(desired_address.clone()),
        ]
    );
    assert!(outcome.live_route_matches);
    let live = admin.replicators.lock().unwrap();
    let repaired = live.get(&desired_address).expect("fresh route installed");
    assert_eq!(repaired.address.as_deref(), Some(desired_address.as_str()));
    assert_eq!(repaired.filters.as_ref(), Some(&mock_live_filters(&filter)));
}

#[tokio::test]
async fn omitted_live_filters_force_current_protocol_repair() {
    let filter = one_filter("AgentRequest", "agent_did", "did:key:mandrake");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
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
            filters: None,
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("current-protocol reconcile tick");
    assert!(!outcome.ops_applied.is_empty());
    assert!(outcome.live_route_matches);
    assert!(!admin.emitted.lock().unwrap().is_empty());
    assert!(!admin.recorded_filters.lock().unwrap().is_empty());
}

/// Current DefraDB HTTP responses omit `Filters` only for an empty effective
/// map. The HTTP adapter projects that as `Some(empty)`, which is authoritative
/// drift for a scoped route and must repair rather than remain permanently
/// unready.
#[tokio::test]
async fn known_empty_live_filters_repair_a_scoped_route() {
    let filter = one_filter("AgentRequest", "agent_did", "did:key:mandrake");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
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
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("repair current-wire empty filter drift");

    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ]
    );
    assert!(outcome.live_route_matches);
}

#[test]
fn addressless_replicator_is_not_aliased_across_multiple_applied_routes() {
    let replicator = RemoteReplicator {
        id: Some("stable-peer".to_string()),
        collections: Vec::new(),
        address: None,
        filters: Some(Default::default()),
    };
    let applied = set(&["stable-peer@127.0.0.1:4100", "stable-peer@127.0.0.1:4200"]);

    assert_eq!(
        canonical_replicator_address(&replicator, &applied),
        Some("stable-peer".to_string())
    );
}

#[tokio::test]
async fn duplicate_remote_routes_merge_their_collection_sets() {
    let admin = MockAdmin::default();
    for (key, id, collection) in [
        ("row-a", "id-a", "AgentRequest"),
        ("row-b", "id-b", "AgentResponse"),
    ] {
        admin.replicators.lock().unwrap().insert(
            key.to_string(),
            RemoteReplicator {
                id: Some(id.to_string()),
                collections: vec![mock_collection_id(collection)],
                address: Some("shared-route".to_string()),
                filters: Some(Default::default()),
            },
        );
    }

    let actual = read_actual(&admin, &BTreeSet::new())
        .await
        .expect("read actual routes");

    assert_eq!(
        actual.state.replicator_collections["shared-route"],
        set(&["AgentRequest", "AgentResponse"])
    );
    assert_eq!(actual.replicator_ids_by_addr["shared-route"], "id-a");
}

#[tokio::test]
async fn applied_state_persist_collapses_duplicates_and_recovers_a_missing_row() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema(
        r#"
        type PeerPairingApplied {
            peer_id: String
            collections: [String!]
            replicator_addresses: [String!]
            replicator_filter: String
            created_at: DateTime
            updated_at: DateTime
        }
        "#,
    )
    .await
    .unwrap();
    for (address, timestamp) in [
        ("old-a", "2026-08-19T00:00:00Z"),
        ("old-b", "2026-08-19T00:01:00Z"),
    ] {
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_PeerPairingApplied(input: {{
                        peer_id: "peer-a",
                        replicator_addresses: ["{address}"],
                        created_at: "{timestamp}",
                        updated_at: "{timestamp}"
                    }}) {{ _docID }}
                }}"#
            ))
            .await;
        assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
    }

    let tempdir = tempfile::tempdir().unwrap();
    let identity = Arc::new(
        crate::identity::KeyIdentity::load_or_create(tempdir.path().join("agent.key"), None)
            .unwrap(),
    );
    let store = GraphqlPairingStateStore::for_explicit_desired(node.clone(), identity);
    let mut applied = store.load_applied("peer-a").await.expect("load duplicates");
    applied.state = PairingApplied {
        replicator_addresses: set(&["fresh"]),
        ..Default::default()
    };
    store
        .persist_applied("peer-a", &applied)
        .await
        .expect("duplicate-tolerant save");

    let response = node
        .execute(
            r#"{
                PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) {
                    _docID
                    replicator_addresses
                }
            }"#,
        )
        .await;
    let mut persisted_rows = rows::<AppliedStateRow>(&response, "PeerPairingApplied").unwrap();
    persisted_rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
    assert_eq!(persisted_rows.len(), 1);
    assert_eq!(
        persisted_rows[0].replicator_addresses,
        Some(vec!["fresh".into()])
    );

    let mut applied = store.load_applied("peer-a").await.expect("reload applied");
    let response = node
        .execute(
            r#"mutation {
                delete_PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "delete failed: {:?}",
        response.errors
    );
    applied.state.replicator_addresses = set(&["recovered"]);
    store
        .persist_applied("peer-a", &applied)
        .await
        .expect("save after concurrent delete");

    let response = node
        .execute(
            r#"{
                PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) {
                    _docID
                    replicator_addresses
                }
            }"#,
        )
        .await;
    let rows = rows::<AppliedStateRow>(&response, "PeerPairingApplied").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].replicator_addresses, Some(vec!["recovered".into()]));
}

#[tokio::test]
async fn stale_applied_endpoint_absent_from_actual_is_torn_down_once() {
    let desired = PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_B]),
        replicator_collections: set(&["AgentRequest"]),
        ..Default::default()
    };
    let store = MockStore::with_desired(Some(desired));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A, TEST_TRANSPORT_ADDRESS_B]),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![TEST_TRANSPORT_PEER_ID.into()]),
        ..Default::default()
    };
    let first = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("first reconcile");
    let second = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("second reconcile");

    assert_eq!(
        first.ops_applied,
        vec![
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_B.into()),
        ]
    );
    assert!(second.ops_applied.is_empty());
    assert_eq!(
        *admin.connects.lock().unwrap(),
        vec![vec![TEST_TRANSPORT_ADDRESS_B.to_string()]]
    );
    assert_eq!(
        *admin.deleted_replicator_collections.lock().unwrap(),
        vec![vec!["AgentRequest".to_string()]]
    );
    assert_eq!(
        store.applied.lock().unwrap().replicator_addresses,
        set(&[TEST_TRANSPORT_ADDRESS_B])
    );
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
    let conversation_filter = one_filter("AgentRequest", "requester_did", "did:key:requester");
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

/// Collection IDs are reverse-resolved before diffing against desired names.
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
async fn teardown_is_restricted_to_applied_extras() {
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
            filters: Some(Default::default()),
        },
    );
    admin.replicators.lock().unwrap().insert(
        "manual-addr".into(),
        RemoteReplicator {
            id: Some("manual-id".into()),
            collections: vec!["manual".into()],
            address: Some("manual-addr".into()),
            filters: Some(Default::default()),
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
            filters: Some(Default::default()),
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
    assert_eq!(
        single_string_eq(pred),
        Some(("requester_did", "did:key:bob"))
    );
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
            crate::agent::p2p_reconcile::templates::equality_filter(
                "requester_did",
                "did:key:alice",
            ),
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
            filters: Some(Default::default()),
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
        last.1.get("AgentRequest").and_then(single_string_eq),
        Some(("requester_did", "did:key:bob"))
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
    let admin = MockAdmin::default();
    let addresses = vec!["addr-a".to_string()];
    let collections: Vec<String> = vec![];

    // Back-compat: empty filters record as empty.
    admin
        .add_replicator(&addresses, &collections, &PairingFilters::default())
        .await
        .expect("add_replicator empty filters");

    {
        let calls = admin.recorded_filters.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].1.is_empty(),
            "empty filters should record as empty"
        );
    }

    // Non-empty filters are faithfully recorded.
    let mut filters = PairingFilters::default();
    filters.insert(
        "AgentRequest".to_string(),
        equality_filter("agent_did", "did:key:alice"),
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
    assert_eq!(single_string_eq(pred), Some(("agent_did", "did:key:alice")));
}

/// #714 C1 regression: the `machine` template's conversation collections
/// must scope to the same requester DID `conversation` uses on the data
/// plane, while `AgentDirectoryEntry` is restricted to this issuer's
/// source-owned projection.
#[test]
fn data_plane_desired_machine_scopes_conversation_and_owned_directory() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: None,
            collections: None,
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("machine".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect("data-plane desired")
    .expect("some data-plane layer");

    assert!(desired
        .replicator_collections
        .contains(crate::agent::p2p_reconcile::templates::AGENT_DIRECTORY_COLLECTION));
    for col in [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
    ] {
        assert_eq!(
            desired
                .replicator_filter
                .get(col)
                .and_then(single_string_eq),
            Some(("requester_did", "did:key:peer-b")),
            "conversation collection {col} must be requester-scoped exactly like `conversation`"
        );
    }
    assert_eq!(
        desired
            .replicator_filter
            .get(crate::agent::p2p_reconcile::templates::AGENT_DIRECTORY_COLLECTION)
            .and_then(single_string_eq),
        Some(("source_did", "did:key:self"))
    );
}

/// #714 C1 regression: on the control plane, `machine`'s conversation
/// collections must resolve to the peer DID exactly like `conversation`
/// does, while `AgentDirectoryEntry` selects only this issuer's rows.
#[test]
fn control_plane_desired_machine_scopes_conversation_and_owned_directory() {
    let desired = desired_from_pairing_row(
        desired_row(Some("machine"), Some("did:key:phone")),
        "did:key:server",
    )
    .expect("template resolves")
    .expect("some desired layer");

    assert!(
        desired.collections.is_empty(),
        "Push templates must not subscribe"
    );
    assert!(desired
        .replicator_collections
        .contains(crate::agent::p2p_reconcile::templates::AGENT_DIRECTORY_COLLECTION));
    for col in [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
    ] {
        let pred = desired
            .replicator_filter
            .get(col)
            .unwrap_or_else(|| panic!("missing filter for conversation collection {col}"));
        assert_eq!(
            single_string_eq(pred),
            Some(("requester_did", "did:key:phone"))
        );
    }
    assert_eq!(
        desired
            .replicator_filter
            .get(crate::agent::p2p_reconcile::templates::AGENT_DIRECTORY_COLLECTION)
            .and_then(single_string_eq),
        Some(("source_did", "did:key:server"))
    );
}
