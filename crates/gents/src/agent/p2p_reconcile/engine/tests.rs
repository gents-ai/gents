use std::sync::Mutex;

use anyhow::anyhow;
use events::Bus;

use super::*;
use crate::agent::p2p_reconcile::{
    FilterPredicate, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteReplicator,
};

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn one_filter(collection: &str, field: &str, value: &str) -> PairingFilters {
    let mut filters = PairingFilters::new();
    filters.insert(
        collection.to_string(),
        crate::agent::p2p_reconcile::FilterPredicate::eq(field, value),
    );
    filters
}

fn bearer_desired(template_id: &str, claimant_did: &str, address: &str) -> PairingDesired {
    let template = resolve_template(template_id).expect("bearer template");
    PairingDesired {
        replicator_addresses: set(&[address]),
        replicator_collections: template
            .collections
            .iter()
            .map(|collection| (*collection).to_string())
            .collect(),
        replicator_filter: scope_filter(
            &template.scope,
            template.collections,
            claimant_did,
            "did:key:issuer",
        ),
        template_ids: set(&[template_id]),
        ..Default::default()
    }
}

#[test]
fn bearer_readiness_requires_exact_applied_conversation_replicator() {
    let desired = bearer_desired("conversation", "did:key:claimant", "iroh-ticket");
    let pending = PairingApplied::default();
    assert_eq!(
        earned_bearer_readiness(Some(&desired), &pending, "did:key:issuer"),
        None
    );

    let applied = PairingApplied {
        replicator_addresses: desired.replicator_addresses.clone(),
        replicator_filter: desired.replicator_filter.clone(),
        ..Default::default()
    };
    assert_eq!(
        earned_bearer_readiness(Some(&desired), &applied, "did:key:issuer"),
        Some((
            "did:key:claimant".to_string(),
            "iroh-ticket".to_string(),
            "conversation".to_string()
        ))
    );

    let mut wrong_filter = applied;
    wrong_filter.replicator_filter.insert(
        "AgentRequest".to_string(),
        FilterPredicate::eq("requester_did", "did:key:someone-else"),
    );
    assert_eq!(
        earned_bearer_readiness(Some(&desired), &wrong_filter, "did:key:issuer"),
        None
    );
}

#[test]
fn bearer_readiness_mutation_escapes_signed_fields() {
    let record = BearerPairingReadyRecord {
        issuer_did: "did:key:issuer".to_string(),
        claimant_did: "did:key:claimant\"quoted".to_string(),
        peer_id: "peer-a".to_string(),
        address: "ticket\\route".to_string(),
        template: "conversation".to_string(),
        acknowledged_at: "2026-07-27T00:00:00Z".to_string(),
        sig: vec![1, 2, 3],
    };

    let mutation = bearer_pairing_ready_upsert_mutation("ready\"key", &record);
    assert!(mutation.contains(r#"readiness_key: "ready\"key""#));
    assert!(mutation.contains(r#"claimant_did: "did:key:claimant\"quoted""#));
    assert!(mutation.contains(r#"address: "ticket\\route""#));
    assert!(!mutation.contains("[]"));
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
            .and_then(FilterPredicate::single_string_eq),
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
fn data_plane_gate_accepts_network_membership_endpoint() {
    let network_entries = vec![NetworkEndpointEntry {
        peer_id: "peer-network".to_string(),
        agent_did: "did:key:network".to_string(),
        address: "/ticket/network".to_string(),
    }];

    let entry = data_plane_materialized_entry_from_sources(
        &network_entries,
        &[],
        "peer-network",
        "did:key:self",
    )
    .expect("network endpoint should pass gate");

    assert_eq!(entry.address, "/ticket/network");
}

#[test]
fn data_plane_gate_accepts_reciprocal_endpoint_without_network_membership() {
    let reciprocal_entries = vec![NetworkEndpointEntry {
        peer_id: "peer-phone".to_string(),
        agent_did: "did:key:phone".to_string(),
        address: "/ticket/phone".to_string(),
    }];

    let entry = data_plane_materialized_entry_from_sources(
        &[],
        &reciprocal_entries,
        "peer-phone",
        "did:key:server",
    )
    .expect("reciprocal endpoint should pass Layer-2 gate without NetworkMembership");

    assert_eq!(entry.agent_did, "did:key:phone");
    assert_eq!(entry.address, "/ticket/phone");
}

#[test]
fn data_plane_gate_rejects_self_endpoint_from_both_sources() {
    let reciprocal_entries = vec![NetworkEndpointEntry {
        peer_id: "peer-self".to_string(),
        agent_did: "did:key:self".to_string(),
        address: "/ticket/self".to_string(),
    }];

    let entry = data_plane_materialized_entry_from_sources(
        &[],
        &reciprocal_entries,
        "peer-self",
        "did:key:self",
    );

    assert!(entry.is_none());
}

#[test]
fn data_plane_desired_uses_signed_endpoint_address_and_requester_did() {
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
            .and_then(FilterPredicate::single_string_eq),
        Some(("requester_did", "did:key:peer-b"))
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

    assert!(!desired.replicator_filter.contains_key("AgentRequest"));
    assert_eq!(desired.replicator_collections, set(&["AgentToolCall"]));
    assert_eq!(
        desired
            .replicator_filter
            .get("AgentToolCall")
            .and_then(FilterPredicate::single_string_eq),
        Some(("spawn_target_did", "did:key:host"))
    );
}

#[test]
fn data_plane_subagent_host_scopes_return_projection_to_signed_requester() {
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
            predicate.single_string_eq(),
            Some(("requester_did", "did:key:coord"))
        );
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
    list_peer_ids_failures: Mutex<usize>,
    list_peer_ids_calls: Mutex<usize>,
    list_peer_ids_retry_started: Option<Arc<tokio::sync::Notify>>,
    list_peer_ids_retry_release: Option<Arc<tokio::sync::Notify>>,
    save_applied_completed: Option<Arc<tokio::sync::Notify>>,
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
            save_applied_completed: None,
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
        if let Some(completed) = &self.save_applied_completed {
            completed.notify_one();
        }
        Ok(())
    }

    async fn delete_applied(&self, _peer_id: &str) -> Result<()> {
        *self.applied.lock().unwrap() = PairingApplied::default();
        *self.deleted.lock().unwrap() += 1;
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

#[async_trait]
impl PairingStateStore for MultiPeerStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(self.desired.get(peer_id).cloned())
    }

    async fn load_applied(&self, peer_id: &str) -> Result<PairingApplied> {
        Ok(self
            .applied
            .lock()
            .unwrap()
            .get(peer_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_applied(&self, peer_id: &str, applied: &PairingApplied) -> Result<()> {
        self.applied
            .lock()
            .unwrap()
            .insert(peer_id.to_string(), applied.clone());
        Ok(())
    }

    async fn delete_applied(&self, peer_id: &str) -> Result<()> {
        self.applied.lock().unwrap().remove(peer_id);
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
            if let Some(installed) = &self.replicator_installed {
                installed.notify_one();
            }
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
async fn cancellation_preempts_in_flight_pairing_sweep_admin_wait() {
    let store = MockStore::with_desired(Some(PairingDesired::default()));
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
        save_applied_completed: Some(convergence_completed.clone()),
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
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
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
    let conversation_filter = one_filter("AgentRequest", "requester_did", "did:key:requester");
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

/// A stable peer id is not enough to prove that the live transport route
/// survived an app relaunch. The phone republishes a signed endpoint with
/// the same peer id and a fresh ticket; if applied still records the old
/// ticket, the tick must dial the fresh address even when `active_peers`
/// contains that peer. Otherwise the subsequent replicator install can
/// reuse the stale route and the response never reaches the relaunched app.
#[tokio::test]
async fn changed_endpoint_redials_active_peer_before_replacing_replicator() {
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr2"]),
        replicator_collections: set(&["AgentRequest"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&["addr1"]),
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
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("changed endpoint reconcile");

    assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr2"]]);
    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::InstallReplicator("addr2".into()),
            DiffOp::TeardownReplicator("addr1".into()),
        ]
    );
    assert_eq!(
        store.applied.lock().unwrap().replicator_addresses,
        set(&["addr2"])
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
    assert_eq!(
        pred.single_string_eq(),
        Some(("requester_did", "did:key:bob"))
    );
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
    let missing = desired_from_pairing_row(desired_row(None, Some("did:key:bob")), "did:key:self")
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
fn subagent_coordinator_template_filters_only_targeted_bridge() {
    let desired = desired_from_pairing_row(
        desired_row(Some("subagent-coordinator"), Some("did:key:host")),
        "did:key:coord",
    )
    .expect("subagent coordinator template resolves")
    .expect("some desired layer");

    assert!(desired.collections.is_empty());
    assert_eq!(desired.replicator_collections, set(&["AgentToolCall"]));
    assert!(!desired.replicator_filter.contains_key("AgentRequest"));
    assert_eq!(
        desired
            .replicator_filter
            .get("AgentToolCall")
            .and_then(FilterPredicate::single_string_eq),
        Some(("spawn_target_did", "did:key:host"))
    );
}

#[test]
fn subagent_host_template_filters_return_projection_to_requester() {
    let desired = desired_from_pairing_row(
        desired_row(Some("subagent-host"), Some("did:key:coord")),
        "did:key:host",
    )
    .expect("subagent host template resolves")
    .expect("some desired layer");

    assert!(desired.collections.is_empty());
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
            predicate.single_string_eq(),
            Some(("requester_did", "did:key:coord"))
        );
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
    assert_eq!(
        pred.single_string_eq(),
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
            crate::agent::p2p_reconcile::templates::FilterPredicate::eq(
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
            .and_then(FilterPredicate::single_string_eq),
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
    use crate::agent::p2p_reconcile::templates::FilterPredicate;

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
        FilterPredicate::eq("agent_did", "did:key:alice"),
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
    assert_eq!(
        pred.single_string_eq(),
        Some(("agent_did", "did:key:alice"))
    );
}

#[test]
fn bearer_readiness_accepts_exact_applied_machine_replicator() {
    let mut desired = bearer_desired("machine", "did:key:claimant", "iroh-ticket");
    desired.template_ids.insert("conversation".to_string());
    let applied = PairingApplied {
        replicator_addresses: desired.replicator_addresses.clone(),
        replicator_filter: desired.replicator_filter.clone(),
        ..Default::default()
    };

    assert_eq!(
        earned_bearer_readiness(Some(&desired), &applied, "did:key:issuer"),
        Some((
            "did:key:claimant".to_string(),
            "iroh-ticket".to_string(),
            "machine".to_string()
        ))
    );
}

/// #714 C1 regression: the `machine` template's conversation collections
/// must scope to the same requester DID `conversation` uses on the data
/// plane, while `AgentDirectoryEntry` is restricted to this issuer's
/// source-owned projection.
#[test]
fn data_plane_desired_machine_scopes_conversation_and_owned_directory() {
    let signed_endpoint = NetworkEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: None,
            collections: None,
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("machine".to_string()),
            replicator_filter: None,
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
                .and_then(FilterPredicate::single_string_eq),
            Some(("requester_did", "did:key:peer-b")),
            "conversation collection {col} must be requester-scoped exactly like `conversation`"
        );
    }
    assert_eq!(
        desired
            .replicator_filter
            .get(crate::agent::p2p_reconcile::templates::AGENT_DIRECTORY_COLLECTION)
            .and_then(FilterPredicate::single_string_eq),
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
            pred.single_string_eq(),
            Some(("requester_did", "did:key:phone"))
        );
    }
    assert_eq!(
        desired
            .replicator_filter
            .get(crate::agent::p2p_reconcile::templates::AGENT_DIRECTORY_COLLECTION)
            .and_then(FilterPredicate::single_string_eq),
        Some(("source_did", "did:key:server"))
    );
}
