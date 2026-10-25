use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use defra_p2p_adapter::{
    ExplicitReplayCapabilityInput, P2PResult, P2pDocumentInfo, P2pDocumentRequest,
    ReplicationFilter, ReplicatorInfo,
};
use tokio::sync::Notify;

use super::route_manager::cleanup_saved_peer_p2p;
use super::supervisor::{probe_p2p_health, repair_saved_peer, saved_peer_needs_repair};
use super::*;
use crate::client::{PeerDirectory, PeerRecord};

#[derive(Default)]
struct RecordingP2P {
    collection_batches: StdRwLock<Vec<Vec<String>>>,
    collection_error: StdRwLock<Option<String>>,
    notify_calls: AtomicUsize,
    local_peer_id_error: StdRwLock<Option<String>>,
    listen_addresses: StdRwLock<Vec<String>>,
    listen_addresses_error: StdRwLock<Option<String>>,
    connected_peers: StdRwLock<Vec<String>>,
    connected_peers_error: StdRwLock<Option<String>>,
    connect_calls: StdRwLock<Vec<String>>,
    connect_error: StdRwLock<Option<String>>,
    add_replicator_calls: StdRwLock<Vec<String>>,
    cleanup_calls: StdRwLock<Vec<String>>,
    replicators: StdRwLock<Vec<ReplicatorInfo>>,
    replicators_error: StdRwLock<Option<String>>,
    sync_status: StdRwLock<serde_json::Value>,
    sync_status_error: StdRwLock<Option<String>>,
}

#[tokio::test]
async fn startup_subscriptions_use_one_batch_with_the_exact_allowed_scope() {
    let p2p = RecordingP2P::default();
    crate::client::schema::subscribe_runtime_collections(&p2p)
        .await
        .unwrap();
    let batches = p2p.collection_batches.read().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(
        batches[0],
        crate::client::schema::subscribed_collection_names()
    );
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "retries a file lock on a blocking-pool thread; the lock has no release notification"
)]
async fn cancelled_shutdown_waiter_keeps_claim_until_node_shutdown_returns() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../gents/tests/fixtures/native_claim_lifetime.json"
    ))
    .expect("generated native claim lifetime fixture");
    let case = fixture
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "client_shutdown_cancel_retains_claim_until_return")
        .expect("Lean client shutdown cancellation case");
    let events = case["events"].as_array().unwrap();
    let expected_events = [
        "native_started",
        "caller_cancelled",
        "native_returned",
        "access_released",
        "lifecycle_guard_released",
        "gate_release_requested",
    ]
    .map(|event| serde_json::Value::String(event.to_owned()));
    assert_eq!(events, expected_events.as_slice());
    let claim_held_for = |event: &str| {
        let index = events
            .iter()
            .position(|candidate| candidate == event)
            .expect("event in generated trace");
        case["observations"][index]["claim_held"]
            .as_bool()
            .expect("claim observation in generated trace")
    };

    let temp = tempfile::tempdir().expect("temporary desktop home");
    let paths = DesktopPaths::from_root(temp.path().join("client"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .expect("start claimed desktop client"),
    );
    let home = paths.root().to_path_buf();
    let data = paths.node_data_dir().to_path_buf();
    let pause = Arc::new(ShutdownTestPause {
        node_shutdown_started: Notify::new(),
        allow_node_shutdown: Notify::new(),
        node_shutdown_returned: Notify::new(),
        allow_access_release: Notify::new(),
    });
    *core.shutdown_test_pause.lock().await = Some(Arc::clone(&pause));

    let shutdown_core = Arc::clone(&core);
    let waiter = tokio::spawn(async move { shutdown_core.shutdown().await });
    tokio::time::timeout(
        Duration::from_secs(10),
        pause.node_shutdown_started.notified(),
    )
    .await
    .expect("single-flight shutdown owner starts node shutdown");
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    drop(core);

    assert_eq!(
        gents::home::lock_store(&home, &data).is_err(),
        claim_held_for("caller_cancelled"),
        "the claim observation after shutdown waiter cancellation"
    );

    pause.allow_node_shutdown.notify_one();
    tokio::time::timeout(
        Duration::from_secs(10),
        pause.node_shutdown_returned.notified(),
    )
    .await
    .expect("native shutdown returns before the claim is released");
    assert_eq!(
        gents::home::lock_store(&home, &data).is_err(),
        claim_held_for("native_returned"),
        "the claim observation after native shutdown return"
    );

    pause.allow_access_release.notify_one();
    let retry_home = home.clone();
    let retry_data = data.clone();
    let reacquired = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match gents::home::lock_store(&retry_home, &retry_data) {
                Ok(lock) => return lock,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("shutdown did not release the claim: {error:#}"),
            }
        }
    })
    .await
    .expect("store lock probe panicked");
    drop(reacquired);
    assert_eq!(
        gents::home::lock_store(&home, &data).is_err(),
        claim_held_for("access_released"),
        "the claim observation after access release"
    );
}

#[tokio::test]
async fn startup_subscription_failure_is_not_swallowed() {
    let p2p = RecordingP2P::default();
    *p2p.collection_error.write().unwrap() = Some("subscription denied".to_string());
    let error = crate::client::schema::subscribe_runtime_collections(&p2p)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("subscription denied"));
    assert_eq!(p2p.collection_batches.read().unwrap().len(), 1);
}

#[allow(dead_code)]
impl RecordingP2P {
    fn notify_calls(&self) -> usize {
        self.notify_calls.load(Ordering::SeqCst)
    }

    fn set_listen_addresses(&self, addrs: Vec<String>) {
        *self
            .listen_addresses
            .write()
            .expect("listen addresses lock poisoned") = addrs;
    }

    fn set_connected_peers(&self, peers: Vec<String>) {
        *self
            .connected_peers
            .write()
            .expect("connected peers lock poisoned") = peers;
    }

    fn set_connected_peers_error(&self, error: Option<&str>) {
        *self
            .connected_peers_error
            .write()
            .expect("connected peers error lock poisoned") = error.map(ToOwned::to_owned);
    }

    fn set_sync_status(&self, status: serde_json::Value) {
        *self.sync_status.write().expect("sync status lock poisoned") = status;
    }

    fn set_sync_status_error(&self, error: Option<&str>) {
        *self
            .sync_status_error
            .write()
            .expect("sync status error lock poisoned") = error.map(ToOwned::to_owned);
    }

    fn connected_peer_snapshot(&self) -> Vec<String> {
        self.connected_peers
            .read()
            .expect("connected peers lock poisoned")
            .clone()
    }

    fn connect_calls(&self) -> Vec<String> {
        self.connect_calls
            .read()
            .expect("connect calls lock poisoned")
            .clone()
    }

    fn set_connect_error(&self, error: Option<&str>) {
        *self
            .connect_error
            .write()
            .expect("connect error lock poisoned") = error.map(ToOwned::to_owned);
    }

    fn add_replicator_calls(&self) -> Vec<String> {
        self.add_replicator_calls
            .read()
            .expect("add replicator calls lock poisoned")
            .clone()
    }

    fn cleanup_calls(&self) -> Vec<String> {
        self.cleanup_calls
            .read()
            .expect("cleanup calls lock poisoned")
            .clone()
    }

    fn set_replicators(&self, replicators: Vec<ReplicatorInfo>) {
        *self.replicators.write().expect("replicators lock poisoned") = replicators;
    }
}

#[async_trait]
impl P2POps for RecordingP2P {
    async fn sync_status(&self) -> P2PResult<serde_json::Value> {
        if let Some(error) = self
            .sync_status_error
            .read()
            .expect("sync status error lock poisoned")
            .clone()
        {
            return Err(error.into());
        }
        Ok(self
            .sync_status
            .read()
            .expect("sync status lock poisoned")
            .clone())
    }

    async fn local_peer_id(&self) -> P2PResult<String> {
        if let Some(error) = self
            .local_peer_id_error
            .read()
            .expect("local peer id error lock poisoned")
            .clone()
        {
            return Err(error.into());
        }
        Ok("local-peer".to_string())
    }

    async fn listen_addresses(&self) -> P2PResult<Vec<String>> {
        if let Some(error) = self
            .listen_addresses_error
            .read()
            .expect("listen addresses error lock poisoned")
            .clone()
        {
            return Err(error.into());
        }
        Ok(self
            .listen_addresses
            .read()
            .expect("listen addresses lock poisoned")
            .clone())
    }

    async fn connected_peers(&self) -> P2PResult<Vec<String>> {
        if let Some(error) = self
            .connected_peers_error
            .read()
            .expect("connected peers error lock poisoned")
            .clone()
        {
            return Err(error.into());
        }
        Ok(self.connected_peer_snapshot())
    }

    async fn connect_peer(&self, addr: &str) -> P2PResult<()> {
        self.connect_calls
            .write()
            .expect("connect calls lock poisoned")
            .push(addr.to_string());
        if let Some(error) = self
            .connect_error
            .read()
            .expect("connect error lock poisoned")
            .clone()
        {
            return Err(error.into());
        }
        self.connected_peers
            .write()
            .expect("connected peers lock poisoned")
            .push(addr.to_string());
        Ok(())
    }

    async fn disconnect_peer(&self, addr: &str) -> P2PResult<()> {
        self.cleanup_calls
            .write()
            .expect("cleanup calls lock poisoned")
            .push(format!("disconnect:{addr}"));
        self.connected_peers
            .write()
            .expect("connected peers lock poisoned")
            .retain(|peer| peer != addr);
        Ok(())
    }

    async fn notify_network_change(&self) -> P2PResult<()> {
        self.notify_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn get_replicators(&self) -> P2PResult<Vec<ReplicatorInfo>> {
        if let Some(error) = self
            .replicators_error
            .read()
            .expect("replicators error lock poisoned")
            .clone()
        {
            return Err(error.into());
        }
        Ok(self
            .replicators
            .read()
            .expect("replicators lock poisoned")
            .clone())
    }

    async fn add_replicator(
        &self,
        _collections: Vec<String>,
        addr: Option<&str>,
        _filters: BTreeMap<String, ReplicationFilter>,
        _explicit_replay_capabilities: Vec<ExplicitReplayCapabilityInput>,
        _expected_authorizer_did: Option<&str>,
    ) -> P2PResult<()> {
        if let Some(addr) = addr {
            self.add_replicator_calls
                .write()
                .expect("add replicator calls lock poisoned")
                .push(addr.to_string());
        }
        Ok(())
    }

    async fn remove_replicator(
        &self,
        collections: Vec<String>,
        addr: Option<&str>,
    ) -> P2PResult<()> {
        self.cleanup_calls
            .write()
            .expect("cleanup calls lock poisoned")
            .push(format!(
                "remove-replicator:{}:{}",
                addr.unwrap_or_default(),
                collections.join(",")
            ));
        Ok(())
    }

    async fn get_collections(&self) -> P2PResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn add_collections(&self, collections: Vec<String>) -> P2PResult<()> {
        self.collection_batches.write().unwrap().push(collections);
        if let Some(error) = self.collection_error.read().unwrap().as_ref() {
            return Err(defra_p2p_adapter::P2PError::Transport(error.clone()));
        }
        Ok(())
    }

    async fn remove_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
        Ok(())
    }

    async fn get_documents(&self) -> P2PResult<Vec<P2pDocumentInfo>> {
        Ok(Vec::new())
    }

    async fn add_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
        Ok(())
    }

    async fn remove_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
        Ok(())
    }

    async fn sync_documents(
        &self,
        _collection_name: &str,
        _doc_ids: Vec<String>,
        _timeout: Option<Duration>,
    ) -> P2PResult<()> {
        Ok(())
    }

    async fn sync_branchable_collection(&self, _collection_id: &str) -> P2PResult<()> {
        panic!(
            "client repair must use database-owned delivery recovery, not collection-wide replay"
        )
    }

    async fn sync_collection_versions(&self, _version_ids: Vec<String>) -> P2PResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn restart_clears_and_defers_persisted_enrollment_until_current_authority() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tmp.path().join("core")),
        ClientCoreOptions::local_only(),
    )
    .await
    .expect("client core");
    let peer_path = tmp.path().join("restart-peers.json");
    let mut directory = PeerDirectory::open_writer(&peer_path).await.unwrap();
    let enrolled = directory
        .upsert_enrollment_peer(
            "server-peer",
            "Enrolled server",
            "iroh://server-ticket",
            "did:key:owner",
            "network-a",
            "request-a",
            "request-digest",
            "did:key:admin",
            7,
            "2099-09-29T00:00:00Z",
        )
        .await
        .unwrap();
    let enrolled = directory
        .set_pairing_ready(&enrolled.peer_id, true)
        .await
        .unwrap()
        .expect("enrollment remains configured");
    assert!(enrolled.pairing_ready);
    drop(directory);

    let owner = super::sync_state::ClientSyncStateOwner::new(
        P2PHealth::default(),
        PeerDirectory::open_writer(&peer_path).await.unwrap(),
        Vec::new(),
    );
    let mut updates = owner.subscribe();
    owner.clear_ephemeral_pairing_readiness().await.unwrap();
    assert!(updates.has_changed().expect("watch remains open"));
    let restarted = updates.borrow_and_update().clone();
    assert_eq!(restarted.directory.len(), 1);
    assert!(!restarted.directory[0].pairing_ready);

    let recording = Arc::new(RecordingP2P::default());
    let p2p: Arc<dyn P2POps> = recording.clone();
    let route_manager = Arc::new(super::route_manager::ClientRouteManager::new(
        core.node_arc(),
        Arc::clone(&p2p),
        Arc::new(core.node_identity().clone()),
        core.options().operator_identity_interaction,
    ));
    let options = ClientCoreOptions {
        install_replicators_on_bootstrap: true,
        ..ClientCoreOptions::local_only()
    };
    let (statuses, errors) = super::bootstrap::bootstrap_saved_peers(
        &core.node_arc(),
        &p2p,
        &owner.records(),
        &options,
        core.node_identity(),
        &route_manager,
        &mut || {},
    )
    .await;
    assert!(errors.is_empty());
    assert_eq!(statuses.len(), 1);
    assert!(!statuses[0].dial_succeeded);
    assert!(statuses[0].pairing.is_empty());
    assert!(statuses[0].routes.is_empty());
    assert!(recording.connect_calls().is_empty());
    assert!(recording.add_replicator_calls().is_empty());
    let persisted = crate::client::load_peer_records(&peer_path).await.unwrap();
    assert_eq!(persisted, restarted.directory);
    assert!(!persisted[0].pairing_ready);

    let core = Arc::new(core);
    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn expired_enrollment_is_persistently_demoted_before_submit_can_write() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tmp.path().to_path_buf()),
        ClientCoreOptions::local_only(),
    )
    .await
    .expect("client core");
    let enrolled = core
        .sync_state
        .upsert_enrollment_peer(
            "server-peer",
            "Expired server",
            "iroh://server-ticket",
            "did:key:owner",
            "network-a",
            "request-a",
            "request-digest",
            "did:key:admin",
            7,
            "2020-01-01T00:00:00Z",
        )
        .await
        .unwrap();
    core.sync_state
        .set_pairing_ready(&enrolled, true)
        .await
        .unwrap()
        .expect("enrollment remains configured");

    let error = core
        .submit_request("session-a", "did:key:owner", "must not persist", None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("route is not ready"));
    assert!(!core.sync_state.records()[0].pairing_ready);
    assert!(core.store().snapshot().requests.is_empty());
    let persisted = crate::client::load_peer_records(core.paths().peer_directory_path())
        .await
        .unwrap();
    assert!(!persisted[0].pairing_ready);

    Arc::new(core).shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn saved_peer_cleanup_removes_replicator_before_idempotent_disconnect() {
    let recording = Arc::new(RecordingP2P::default());
    let p2p: Arc<dyn P2POps> = recording.clone();
    let record = PeerRecord::new(
        "Never Connected",
        "127.0.0.1:56000/p2p/peer-absent",
        "did:test:absent",
    );

    cleanup_saved_peer_p2p(&p2p, &record)
        .await
        .expect("absent P2P state should clean up idempotently");

    let calls = recording.cleanup_calls();
    assert_eq!(calls.len(), 3);
    assert!(calls[0].starts_with(&format!("remove-replicator:{}:", record.addr)));
    assert!(calls[1].starts_with(&format!("remove-replicator:{}:", record.addr)));
    assert_eq!(calls[2], format!("disconnect:{}", record.addr));
    for collection in gents::agent::p2p_reconcile::client_route_collections(
        gents::agent::p2p_reconcile::PairingDirection::ClientToRuntime,
    ) {
        assert!(calls[0].contains(collection));
    }
    for collection in crate::client::schema::subscribed_collection_names() {
        assert!(calls[1].contains(collection));
    }
}

#[tokio::test]
async fn remove_peer_hides_deployment_and_persists_cleanup_tombstone_when_offline() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tmp.path().to_path_buf()),
        ClientCoreOptions::local_only(),
    )
    .await
    .expect("client core");
    let record = PeerRecord::new(
        "Invalid Address",
        "not-a-valid-p2p-address",
        "did:test:invalid\"quoted",
    );
    core.sync_state
        .upsert_for_test(record.clone())
        .await
        .expect("save invalid peer fixture");
    core.update_peer_status(ClientPeerStatus {
        peer_id: record.peer_id.clone(),
        label: record.label.clone(),
        node_did: record.node_did.clone(),
        addr: record.addr.clone(),
        dial_succeeded: true,
        last_error: None,
        pairing: Vec::new(),
        routes: Vec::new(),
    });
    assert!(core
        .sync_state()
        .peers
        .iter()
        .any(|status| status.peer_id == record.peer_id));
    let route = gents::agent::p2p_reconcile::ClientRouteIdentity::new(
        record.peer_id.clone(),
        "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
        "did:key:desktop-requester",
        record.node_did.clone(),
    )
    .expect("valid route fixture");
    let lifecycle = core.route_manager.lock().await;
    let manual_write = lifecycle
        .upsert_desired(
            &route,
            gents::agent::p2p_reconcile::PairingDirection::RuntimeToClient,
        )
        .await;
    assert!(manual_write
        .expect_err("manual desired-state authoring must fail closed")
        .to_string()
        .contains("manual PeerPairingDesired authoring is disabled"));
    drop(lifecycle);

    let result = core
        .remove_peer(&record.peer_id)
        .await
        .expect("offline cleanup must not block local removal");
    let message = result.warning.expect("pending cleanup warning");
    assert!(
        message.contains("route teardown is pending and will retry"),
        "{message}"
    );
    assert!(!core
        .peer_records()
        .await
        .iter()
        .any(|saved| saved.peer_id == record.peer_id));
    assert!(core
        .sync_state
        .pending_removals()
        .await
        .iter()
        .any(|pending| pending.peer_id == record.peer_id));
    assert!(!core
        .sync_state()
        .peers
        .iter()
        .any(|status| status.peer_id == record.peer_id));
    let peer_id = gents_protocol::graphql::escape_graphql_string(
        &route.desired_id(gents::agent::p2p_reconcile::PairingDirection::RuntimeToClient),
    );
    let response = core
        .node()
        .execute(&format!(
            r#"query {{ PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{ _docID node_did replicator_addresses template source }} }}"#
        ))
        .await;
    assert!(!response.has_errors());
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("PeerPairingDesired"))
        .and_then(|rows| rows.as_array())
        .expect("saved pairing desired rows");
    assert!(
        rows.is_empty(),
        "failed manual authoring and offline removal must not leave alternate desired authority"
    );

    Arc::new(core).shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn repair_saved_peer_refreshes_network_before_redial() {
    let recording = Arc::new(RecordingP2P::default());
    let p2p: Arc<dyn P2POps> = recording.clone();
    let record = PeerRecord::new(
        "Workshop Bay",
        "127.0.0.1:56000/p2p/peer-alpha",
        "did:test:workshop-bay",
    );

    let repaired = repair_saved_peer(
        &events::ChannelBus::new(),
        &p2p,
        &record,
        Some(ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            node_did: record.node_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: false,
            last_error: Some("peer Workshop Bay dial failed".to_string()),
            pairing: Vec::new(),
            routes: Vec::new(),
        }),
        "did:key:desktop",
        false,
        false,
    )
    .await;

    assert_eq!(recording.notify_calls(), 1);
    assert_eq!(recording.connect_calls(), vec![record.addr.clone()]);
    assert!(repaired.dial_succeeded);
    assert_eq!(repaired.last_error, None);
}

#[tokio::test]
async fn failed_redial_clears_stale_configured_peer_connectivity() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_connect_error(Some("peer unavailable"));
    let p2p: Arc<dyn P2POps> = recording;
    let record = PeerRecord::new(
        "Workshop Bay",
        "127.0.0.1:56000/p2p/peer-alpha",
        "did:test:workshop-bay",
    );

    let repaired = repair_saved_peer(
        &events::ChannelBus::new(),
        &p2p,
        &record,
        Some(ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            node_did: record.node_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
        }),
        "did:key:desktop",
        false,
        false,
    )
    .await;

    assert!(!repaired.dial_succeeded);
    assert!(repaired
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("dial failed")));
}

#[tokio::test]
async fn repair_saved_graphql_peer_leaves_replicator_installation_to_route_manager() {
    let recording = Arc::new(RecordingP2P::default());
    let mut record = PeerRecord::new(
        "Workshop Bay",
        "127.0.0.1:56000/p2p/peer-alpha",
        "did:test:workshop-bay",
    );
    record.graphql = Some("http://100.69.4.79:9191/api/v0/graphql".to_string());
    recording.set_connected_peers(vec![record.addr.clone()]);
    let p2p: Arc<dyn P2POps> = recording.clone();

    let repaired = repair_saved_peer(
        &events::ChannelBus::new(),
        &p2p,
        &record,
        Some(ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            node_did: record.node_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
        }),
        "did:key:desktop",
        true,
        true,
    )
    .await;

    assert_eq!(recording.notify_calls(), 1);
    assert_eq!(recording.connect_calls(), vec![record.addr.clone()]);
    assert!(
        recording.add_replicator_calls().is_empty(),
        "the route manager, not transport repair, owns replicator installation"
    );
    assert!(repaired.dial_succeeded);
    assert_eq!(repaired.last_error, None);
}

#[tokio::test]
async fn saved_peer_needs_repair_when_live_connection_has_dropped() {
    let recording = Arc::new(RecordingP2P::default());
    let p2p: Arc<dyn P2POps> = recording.clone();
    let record = PeerRecord::new(
        "Workshop Bay",
        "127.0.0.1:56000/p2p/peer-alpha",
        "did:test:workshop-bay",
    );

    let needs_repair = saved_peer_needs_repair(
        &p2p,
        &record,
        Some(&ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            node_did: record.node_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
        }),
    )
    .await;

    assert!(needs_repair);
}

#[tokio::test]
async fn saved_peer_does_not_need_repair_while_live_connection_is_healthy() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_connected_peers(vec!["127.0.0.1:56000/p2p/peer-alpha".to_string()]);
    let p2p: Arc<dyn P2POps> = recording.clone();
    let record = PeerRecord::new(
        "Workshop Bay",
        "127.0.0.1:56000/p2p/peer-alpha",
        "did:test:workshop-bay",
    );

    let needs_repair = saved_peer_needs_repair(
        &p2p,
        &record,
        Some(&ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            node_did: record.node_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
        }),
    )
    .await;

    assert!(!needs_repair);
}

#[tokio::test]
async fn probe_p2p_health_reports_healthy_transport() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
    recording.set_connected_peers(vec!["127.0.0.1:56000/p2p/peer-alpha".to_string()]);
    recording.set_replicators(vec![ReplicatorInfo {
        id: Some("peer-alpha".to_string()),
        collections: vec!["AgentRequest".to_string()],
        address: Some("127.0.0.1:56000/p2p/peer-alpha".to_string()),
        filters: BTreeMap::new(),
        status: Some(0),
        last_status_change: Some("0001-01-01T00:00:00Z".to_string()),
    }]);
    let p2p: Arc<dyn P2POps> = recording;

    let (health, database_sync, database_sync_error) =
        probe_p2p_health(&p2p, &P2PHealth::default(), None, None).await;

    assert_eq!(health.status, P2PHealthStatus::Healthy);
    assert_eq!(health.connected_peer_count, 1);
    assert_eq!(health.replicator_count, 1);
    assert_eq!(health.consecutive_failures, 0);
    assert_eq!(health.last_error, None);
    assert!(health.last_ok_at.is_some());
    assert!(database_sync.is_none());
    assert!(database_sync_error.is_none());
}

#[tokio::test]
async fn probe_p2p_health_carries_database_sync_facts_without_rebuilding_them() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
    recording.set_connected_peers(vec!["127.0.0.1:56000/p2p/peer-alpha".to_string()]);
    recording.set_sync_status(
        serde_json::to_value(P2pSyncStatusSnapshot {
            pending_dags: 2,
            persisted_pending_dags: 3,
            pending_dag_fetch_exhausted: 7,
            pending_dag_terminal_merged: 5,
            pending_dag_terminal_quarantined: 1,
            quarantined_pending_dags: 1,
            push_backlog: gents::P2pPushBacklogSnapshot {
                queued_items: 4,
                active_jobs: 2,
                ..Default::default()
            },
            push_retry_markers: gents::P2pPushRetryMarkerSnapshot {
                document_markers: 6,
                collection_markers: 1,
                scheduled_peers: 2,
                oldest_scheduled_retry_unix: Some(123),
            },
            ..P2pSyncStatusSnapshot::default()
        })
        .unwrap(),
    );
    let p2p: Arc<dyn P2POps> = recording;

    let (_, database_sync, database_sync_error) =
        probe_p2p_health(&p2p, &P2PHealth::default(), None, None).await;
    let database_sync = database_sync.expect("database sync status");
    assert!(database_sync_error.is_none());

    assert_eq!(database_sync.pending_dags, 2);
    assert_eq!(database_sync.persisted_pending_dags, 3);
    assert_eq!(database_sync.pending_dag_fetch_exhausted, 7);
    assert_eq!(database_sync.quarantined_pending_dags, 1);
    assert_eq!(database_sync.push_backlog.queued_items, 4);
    assert_eq!(database_sync.push_retry_markers.document_markers, 6);
}

#[tokio::test]
async fn probe_p2p_health_reports_an_incomplete_database_status_without_degrading_transport() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
    recording.set_sync_status(serde_json::json!({ "pending_dags": 0 }));
    let p2p: Arc<dyn P2POps> = recording;

    let (health, database_sync, database_sync_error) =
        probe_p2p_health(&p2p, &P2PHealth::default(), None, None).await;

    assert_eq!(health.status, P2PHealthStatus::Healthy);
    assert_eq!(health.consecutive_failures, 0);
    assert!(health.last_error.is_none());
    assert!(database_sync_error
        .as_deref()
        .is_some_and(|error| error.contains("invalid database sync status")));
    assert!(database_sync.is_none());
}

#[tokio::test]
async fn probe_p2p_health_reports_database_status_read_failure_without_degrading_transport() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
    recording.set_sync_status_error(Some("diagnostic unavailable"));
    let p2p: Arc<dyn P2POps> = recording;

    let previous = P2PHealth {
        status: P2PHealthStatus::Healthy,
        consecutive_failures: 0,
        ..P2PHealth::default()
    };
    let (health, database_sync, database_sync_error) =
        probe_p2p_health(&p2p, &previous, None, None).await;

    assert_eq!(health.status, P2PHealthStatus::Healthy);
    assert_eq!(health.consecutive_failures, 0);
    assert!(health.last_error.is_none());
    assert!(database_sync.is_none());
    assert!(database_sync_error
        .as_deref()
        .is_some_and(|error| error.contains("failed to read database sync status")));
}

#[tokio::test]
async fn probe_p2p_health_marks_repeated_failures_wedged() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
    recording.set_connected_peers_error(Some("channel send error"));
    let p2p: Arc<dyn P2POps> = recording;

    let mut health = P2PHealth::default();
    for _ in 0..P2P_WEDGED_FAILURE_THRESHOLD {
        (health, _, _) = probe_p2p_health(&p2p, &health, None, None).await;
    }

    assert_eq!(health.status, P2PHealthStatus::Wedged);
    assert_eq!(health.consecutive_failures, P2P_WEDGED_FAILURE_THRESHOLD);
    assert_eq!(
        health.last_error.as_deref(),
        Some("reading desktop P2P connected peers")
    );
    assert!(health.last_failure_at.is_some());
}

#[test]
fn p2p_health_materially_changed_ignores_probe_timestamps() {
    let previous = P2PHealth {
        status: P2PHealthStatus::Healthy,
        consecutive_failures: 0,
        connected_peer_count: 1,
        replicator_count: 1,
        last_error: None,
        last_ok_at: Some(SystemTime::UNIX_EPOCH),
        last_failure_at: None,
    };
    let next = P2PHealth {
        last_ok_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2)),
        ..previous.clone()
    };

    assert!(!p2p_health_materially_changed(&previous, &next));
}

#[tokio::test]
async fn selected_node_did_channel_updates_subscribers() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let options = ClientCoreOptions::local_only();
    let core = ClientCore::start_with_paths_and_options(paths, options)
        .await
        .expect("client core");

    let mut rx = core.selected_node_did_rx();
    assert_eq!(rx.borrow().clone(), None);

    core.set_selected_node_did(Some("did:alpha".to_string()));
    rx.changed().await.expect("watch update");
    assert_eq!(rx.borrow().clone(), Some("did:alpha".to_string()));

    core.set_selected_node_did(None);
    rx.changed().await.expect("watch update");
    assert_eq!(rx.borrow().clone(), None);

    Arc::new(core).shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn refresh_store_succeeds_with_selection_set() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let options = ClientCoreOptions::local_only();
    let core = ClientCore::start_with_paths_and_options(paths, options)
        .await
        .expect("client core");

    core.refresh_store().await.expect("refresh full");

    core.set_selected_node_did(Some("did:any".to_string()));
    core.refresh_store().await.expect("refresh scoped");

    Arc::new(core).shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn resending_an_unknown_enrollment_request_fails_without_pushing() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core");

    let error = core
        .resend_status_enrollment("enroll-missing")
        .await
        .expect_err("only a persisted request of this desktop is resent");
    assert!(
        error
            .to_string()
            .contains("no local enrollment request enroll-missing"),
        "{error:#}"
    );
    assert!(core
        .active_status_enrollment_requests()
        .await
        .unwrap()
        .is_empty());

    Arc::new(core).shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn failed_selection_refresh_schedules_scoped_observer_recovery() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
            .await
            .expect("client core"),
    );
    let observer = core.observer.lock().await.take().expect("observer owner");
    observer.shutdown().await;
    let _ = core.store.take_resync_requests();

    core.node.shutdown().await;
    let error = core
        .refresh_node("did:test:selection-recovery")
        .await
        .expect_err("a stopped local database cannot refresh the selected scope");
    assert!(!error.to_string().is_empty());

    let (full, scopes) = core.store.take_resync_requests();
    assert!(!full, "the failed selected refresh remains scoped");
    assert_eq!(scopes, vec!["did:test:selection-recovery"]);

    core.shutdown().await.expect("shutdown client core");
}

#[test]
fn p2p_health_materially_changed_detects_live_topology_change() {
    let previous = P2PHealth {
        status: P2PHealthStatus::Healthy,
        consecutive_failures: 0,
        connected_peer_count: 1,
        replicator_count: 1,
        last_error: None,
        last_ok_at: Some(SystemTime::UNIX_EPOCH),
        last_failure_at: None,
    };
    let next = P2PHealth {
        connected_peer_count: 2,
        ..previous.clone()
    };

    assert!(p2p_health_materially_changed(&previous, &next));
}

#[tokio::test]
async fn observer_metrics_returns_snapshot() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core");

    let metrics = core.observer_metrics().await;
    assert!(
        metrics.is_some(),
        "observer should be running and expose metrics"
    );

    let core = Arc::new(core);
    core.shutdown().await.expect("shutdown");

    let metrics = core.observer_metrics().await;
    assert!(
        metrics.is_none(),
        "observer metrics should be None after shutdown"
    );
}

#[tokio::test]
async fn runtime_schema_skew_refuses_enrollment_and_projects_incompatible_sync() {
    use crate::client::paths::DesktopPaths;
    use crate::client::{project_sync_health, SyncHealthState};
    use gents_protocol::enrollment::{encode_offer, EnrollmentOfferRecord};
    use gents_protocol::peer_schema::{ReplicatedSchema, STATUS_REPLICATED_SCHEMA_FIELD};

    let runtime_did = "did:key:runtime";
    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_simulated_route())
            .await
            .expect("core");
    let local: ReplicatedSchema =
        gents::agent::p2p_reconcile::read_client_replicated_schema(core.node_arc())
            .await
            .expect("read local collection versions");
    assert_eq!(
        local.len(),
        gents::agent::p2p_reconcile::CLIENT_COLLECTIONS.len(),
        "the desktop node registers every client route collection"
    );
    let mut next_release = local.clone();
    next_release
        .get_mut("AgentSession")
        .expect("client route replicates AgentSession")
        .version_id = "bafy-next-release".to_string();

    let offer = encode_offer(&EnrollmentOfferRecord {
        version: gents_protocol::enrollment::ENROLLMENT_PROTOCOL_VERSION,
        offer_id: "offer-1".into(),
        challenge: "challenge".into(),
        network_id: "network".into(),
        admin_did: runtime_did.into(),
        server_peer: "peer".into(),
        server_ticket: "ticket".into(),
        owner_agent: runtime_did.into(),
        profile: "client".into(),
        schema_fingerprint: gents_protocol::enrollment::enrollment_schema_fingerprint(),
        issued_at: "2026-09-24T00:00:00Z".into(),
        expires_at: "2026-09-24T00:05:00Z".into(),
        admin_sig: vec![0; 64],
    })
    .expect("encode offer");
    let mut changed_request = local.clone();
    changed_request
        .get_mut("AgentRequest")
        .expect("client route replicates AgentRequest")
        .version_id = "bafy-other-request-schema".to_string();
    assert_eq!(
        changed_request.get("AgentOutputSegment"),
        local.get("AgentOutputSegment")
    );
    let request_skew = serde_json::json!({
        "node_did": runtime_did,
        "enrollment": { "token": &offer },
        STATUS_REPLICATED_SCHEMA_FIELD: changed_request,
    });
    let error = format!(
        "{:#}",
        core.request_status_enrollment(&request_skew)
            .await
            .unwrap_err()
    );
    assert!(
        error.contains("refusing to enroll with an incompatible runtime")
            && error.contains("AgentRequest"),
        "{error}"
    );
    let skewed = serde_json::json!({
        "node_did": runtime_did,
        "enrollment": { "token": offer },
        STATUS_REPLICATED_SCHEMA_FIELD: next_release,
    });
    let error = format!(
        "{:#}",
        core.request_status_enrollment(&skewed).await.unwrap_err()
    );
    assert!(
        error.contains("update the app and the runtime to the same version")
            && error.contains("AgentSession"),
        "{error}"
    );
    assert!(
        core.sync_state().peer_schema_skew.is_empty(),
        "an unconfigured runtime leaves sync health alone"
    );

    core.add_local_standard_peer_for_test(runtime_did)
        .await
        .expect("configured runtime peer");
    let observation = core
        .begin_runtime_schema_observation(runtime_did, "http://127.0.0.1:56001/graphql")
        .expect("configured route");
    core.finish_runtime_schema_observation(&observation, &skewed)
        .await
        .unwrap_err();
    let health = project_sync_health(&core.sync_state()).expect("skew is visible");
    assert_eq!(health.state, SyncHealthState::Incompatible);
    assert!(health
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("AgentSession")));

    let matching = serde_json::json!({
        "node_did": runtime_did,
        STATUS_REPLICATED_SCHEMA_FIELD: local,
    });
    let observation = core
        .begin_runtime_schema_observation(runtime_did, "http://127.0.0.1:56001/graphql")
        .expect("configured route");
    core.finish_runtime_schema_observation(&observation, &matching)
        .await
        .expect("same collection versions are compatible");
    assert!(core.sync_state().peer_schema_skew.is_empty());

    Arc::new(core).shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn the_client_store_is_encrypted_with_its_recorded_key() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core =
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .expect("fresh client core");
    let core = Arc::new(core);
    core.shutdown().await.expect("shutdown");
    drop(core);
    let record: gents::store_key::StoreEncryption =
        serde_json::from_slice(&std::fs::read(paths.store_encryption_path()).unwrap()).unwrap();
    assert_eq!(record.custody, gents::store_key::StoreKeyCustody::File);
    assert!(paths.store_key_path().is_file());

    let core =
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .expect("reopened client core");
    let core = Arc::new(core);
    core.shutdown().await.expect("shutdown");
    drop(core);

    std::fs::remove_file(paths.store_key_path()).unwrap();
    let error = match ClientCore::start_with_paths_and_options(
        paths.clone(),
        ClientCoreOptions::local_only(),
    )
    .await
    {
        Ok(_) => panic!("a store without its key must not open"),
        Err(error) => error,
    };
    assert_eq!(
        gents::storage_backend::incompatible_store(&error, paths.node_data_dir())
            .map(|store| store.kind),
        Some(gents::storage_backend::IncompatibleStoreKind::MissingStoreKey)
    );
    assert!(
        !paths.store_key_path().exists(),
        "no replacement key is created"
    );
}

#[tokio::test]
async fn bootstrap_connect_waits_for_the_peer_connection_event() {
    use events::Bus;

    const PEER_ID: &str = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
    let p2p: Arc<dyn P2POps> = Arc::new(RecordingP2P::default());
    let bus = Arc::new(events::ChannelBus::new());
    let publisher = Arc::clone(&bus);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        publisher.publish(events::Message::peer_connection(
            events::PeerConnectionData {
                peer_id: PEER_ID.to_string(),
                connected: true,
            },
        ));
    });

    tokio::time::timeout(
        Duration::from_secs(5),
        super::bootstrap::connect_peer_with_retry_until(
            bus.as_ref(),
            &p2p,
            &format!("127.0.0.1:56091/p2p/{PEER_ID}"),
            "Workshop Bay",
            Duration::from_secs(30),
        ),
    )
    .await
    .expect("the connection event ends the wait")
    .expect("peer connected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hosted_enrollment_chat_commits_to_owner_without_replica_delivery() -> anyhow::Result<()> {
    use gents::NodeIdentity;
    use wiremock::{
        matchers::{body_string_contains, method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("runtime");
    std::fs::create_dir(&home)?;
    let key = home.join("node.key");
    let owner = gents::identity::KeyIdentity::load_or_create(&key, None)?;
    let server = MockServer::start().await;
    let graphql = format!("{}/api/v0/graphql", server.uri());
    std::fs::write(
        home.join("init.json"),
        serde_json::to_vec(&serde_json::json!({
            "node_name":"owner", "node_did":owner.did(), "key_path":key,
        }))?,
    )?;
    std::fs::write(
        home.join("runtime.json"),
        serde_json::to_vec(&serde_json::json!({
            "node_name":"owner", "node_did":owner.did(), "graphql":graphql,
            "p2p_transport":"iroh", "p2p_peer_id":"owner-peer",
        }))?,
    )?;
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .and(body_string_contains("create_AgentRequest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data":{"create_AgentRequest":[{"_docID":"owner-request"}]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":17})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx/17"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .and(body_string_contains("AgentSession"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data":{"AgentSession":[]}
        })))
        .mount(&server)
        .await;
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(temp.path().join("client")),
            ClientCoreOptions::local_simulated_route(),
        )
        .await?,
    );
    core.add_managed_enrollment_peer_for_test(owner.did(), &graphql, home.to_str().unwrap(), 1)
        .await?;
    let submitted = core
        .submit_request("session", owner.did(), "send directly", Some("agent"))
        .await?;
    assert_eq!(
        core.store().focused_request_id(),
        Some(submitted.request_id.clone())
    );
    let response = gents::ConfigAccess::local(core.node_arc())
        .execute(&format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{request_id}} }}",
            gents::graphql::escape_graphql_string(&submitted.request_id),
        ))
        .await?;
    assert_eq!(
        response["data"]["AgentRequest"],
        serde_json::json!([]),
        "a successful owner write must not also create a replica request"
    );
    let requests = server.received_requests().await.unwrap();
    let mutation: serde_json::Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|request| String::from_utf8_lossy(&request.body).contains("create_AgentRequest"))
            .unwrap()
            .body,
    )?;
    let query = mutation["query"].as_str().unwrap();
    assert!(
        query.contains(core.node_identity().did()),
        "retain the enrolled desktop signer/requester"
    );
    assert!(
        query.contains("enrollment"),
        "retain signed enrollment admission"
    );
    let local = gents::ConfigAccess::local(core.node_arc());
    let escaped_owner = gents::graphql::escape_graphql_string(owner.did());
    let escaped_requester = gents::graphql::escape_graphql_string(core.node_identity().did());
    let created_at = gents::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let stale = local
        .write(
            "seed_stale_request",
            &format!(
                r#"mutation {{ create_AgentRequest(input: {{
                    request_id: "stale-seed", session_id: "stale-session",
                    node_did: "{escaped_owner}", requester_did: "{escaped_requester}",
                    agent_id: "agent", content: "resend directly", purpose: "normal",
                    lifecycle_state: "dead", failure_reason: "Stale", created_at: "{created_at}"
                }}) {{ _docID }} }}"#
            ),
        )
        .await?;
    let session = gents_protocol::session::AgentSession {
        session_id: "stale-session".into(),
        node_did: owner.did().into(),
        requester_did: Some(core.node_identity().did().into()),
        agent_id: "agent".into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        provenance: None,
        tags: Vec::new(),
        observation: Some(gents_protocol::session::SessionObservation {
            last_activity_at: chrono::Utc::now().to_rfc3339(),
            preview: None,
            latest_request: Some(gents_protocol::session::SessionRequestObservation {
                request_doc_id: gents::graphql::created_doc_id(&stale, "AgentRequest")?,
                request_id: "stale-seed".into(),
                lifecycle_state: gents_protocol::request_lifecycle::RequestLifecycleState::Dead,
            }),
        }),
    };
    let input = serde_json::to_value(session)?;
    local
        .transact_write("seed_stale_session", |txn| {
            let variables = serde_json::json!({ "input": input.clone() });
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                    &variables,
                )
                .await
            })
        })
        .await?;
    core.refresh_store().await?;
    core.add_managed_enrollment_peer_for_test(owner.did(), &graphql, home.to_str().unwrap(), 1)
        .await?;
    let resent = core
        .resend_request_in_scope("stale-seed", Some(owner.did()))
        .await?;
    let response = local
        .execute(&format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{request_id}} }}",
            gents::graphql::escape_graphql_string(&resent.request_id),
        ))
        .await?;
    assert_eq!(response["data"]["AgentRequest"], serde_json::json!([]));
    let requests = server.received_requests().await.unwrap();
    let mutation: serde_json::Value = serde_json::from_slice(
        &requests
            .iter()
            .rev()
            .find(|request| String::from_utf8_lossy(&request.body).contains("create_AgentRequest"))
            .unwrap()
            .body,
    )?;
    let query = mutation["query"].as_str().unwrap();
    assert!(query.contains("stale-seed"), "preserve resend lineage");
    assert!(query.contains(core.node_identity().did()));
    assert!(query.contains("enrollment"));
    let record = core
        .sync_state
        .records()
        .into_iter()
        .find(|record| record.node_did == owner.did())
        .unwrap();
    core.sync_state.set_pairing_ready(&record, false).await?;
    let denied = core
        .submit_request(
            "denied-session",
            owner.did(),
            "must not fall back",
            Some("agent"),
        )
        .await;
    assert!(
        denied
            .as_ref()
            .is_err_and(|error| error.to_string().contains("route is not ready")),
        "the direct owner route must not bypass the existing write fence: {denied:?}"
    );
    let denied_resend = core
        .resend_request_in_scope("stale-seed", Some(owner.did()))
        .await;
    assert!(
        denied_resend
            .as_ref()
            .is_err_and(|error| error.to_string().contains("route is not ready")),
        "resend must preserve the write fence: {denied_resend:?}"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| {
                String::from_utf8_lossy(&request.body).contains("create_AgentRequest")
            })
            .count(),
        2
    );
    let response = gents::ConfigAccess::local(core.node_arc())
        .execute("{ AgentRequest { request_id } }")
        .await?;
    assert_eq!(
        response["data"]["AgentRequest"],
        serde_json::json!([{ "request_id": "stale-seed" }])
    );
    core.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn terminal_request_refresh_retains_page_rows_and_quiesces() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tmp.path().to_path_buf()),
            ClientCoreOptions::local_only(),
        )
        .await?,
    );
    let observer = core.observer.lock().await.take().expect("observer");
    observer.shutdown().await;
    let owner = core.node_identity.did().to_owned();
    let access = gents::ConfigAccess::local(core.node_arc());
    let now = chrono::Utc::now().to_rfc3339();
    let mut docs = Vec::new();
    for id in ["older", "latest"] {
        let input = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "request_id":id,"session_id":"terminal-page","node_did":owner,
            "agent_id":"agent","purpose":"normal","content":"fixture",
            "lifecycle_state":"completed","created_at":now,
        }))?;
        let result = access
            .write(
                "test.terminal_page.request",
                &format!("mutation {{ create_AgentRequest(input:{input}) {{_docID}} }}"),
            )
            .await?;
        docs.push(gents::graphql::created_doc_id(&result, "AgentRequest")?);
    }
    let input = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "session_id":"terminal-page","node_did":owner,"agent_id":"agent",
        "created_at":now,"observation":{"last_activity_at":now,"latest_request":{
            "request_doc_id":docs[1],"request_id":"latest","lifecycle_state":"completed"
        }}
    }))?;
    access
        .write(
            "test.terminal_page.session",
            &format!("mutation {{ create_AgentSession(input:{input}) {{_docID}} }}"),
        )
        .await?;
    core.refresh_node(&owner).await?;
    let first = core
        .refresh_local_request(&owner, "terminal-page", Some("latest"), None, &docs)
        .await?
        .expect("first projection");
    assert!(first.published_version.is_some());
    assert_eq!(first.requests.len(), 2);
    let second = core
        .refresh_local_request(&owner, "terminal-page", Some("latest"), None, &docs)
        .await?
        .expect("unchanged projection");
    assert!(
        second.published_version.is_none(),
        "unchanged terminal page must not publish another refresh: {:?}",
        second.published_version
    );
    assert!(core
        .store
        .snapshot()
        .request_row_by_doc_id(&docs[0])
        .is_some());
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_into_a_node_owned_session_is_refused_before_it_is_written() -> anyhow::Result<()> {
    use gents::NodeIdentity;
    use wiremock::{
        matchers::{body_string_contains, method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("runtime");
    std::fs::create_dir(&home)?;
    let key = home.join("node.key");
    let owner = gents::identity::KeyIdentity::load_or_create(&key, None)?;
    let server = MockServer::start().await;
    let graphql = format!("{}/api/v0/graphql", server.uri());
    std::fs::write(
        home.join("init.json"),
        serde_json::to_vec(&serde_json::json!({
            "node_name":"owner", "node_did":owner.did(), "key_path":key,
        }))?,
    )?;
    std::fs::write(
        home.join("runtime.json"),
        serde_json::to_vec(&serde_json::json!({
            "node_name":"owner", "node_did":owner.did(), "graphql":graphql,
            "p2p_transport":"iroh", "p2p_peer_id":"owner-peer",
        }))?,
    )?;
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .and(body_string_contains("create_AgentRequest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data":{"create_AgentRequest":[{"_docID":"owner-request"}]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":17})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx/17"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .and(body_string_contains("AgentSession"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data":{"AgentSession":[]}
        })))
        .mount(&server)
        .await;
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(temp.path().join("client")),
            ClientCoreOptions::local_simulated_route(),
        )
        .await?,
    );
    core.add_managed_enrollment_peer_for_test(owner.did(), &graphql, home.to_str().unwrap(), 1)
        .await?;
    let session = |session_id: &str, requester: &str, started_by: Option<&str>| {
        gents_protocol::session::AgentSession {
            session_id: session_id.into(),
            node_did: owner.did().to_string(),
            requester_did: Some(requester.to_string()),
            agent_id: "agent".into(),
            created_at: "2026-10-22T00:00:00Z".into(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: Vec::new(),
            provenance: started_by.map(|parent| gents_protocol::session::SessionProvenance {
                parent_request_doc_id: Some(parent.to_string()),
                ..Default::default()
            }),
            observation: None,
        }
    };
    let sender = core.node_identity().did().to_string();
    core.store()
        .merge_snapshot(crate::client::ClientStore::from_rows(
            crate::client::store::ClientStoreRows {
                sessions: vec![
                    session("worker", owner.did(), Some("parent-request")),
                    session("own", &sender, None),
                ],
                ..Default::default()
            },
        ));

    assert!(
        core.session_read_through_operator("worker", owner.did()),
        "a node-owned session is read through the node, not hydrated"
    );
    assert!(!core.session_read_through_operator("own", owner.did()));

    let error = core
        .submit_request("worker", owner.did(), "hello", Some("agent"))
        .await
        .expect_err("a send into a node-owned session must be refused");
    let refusal = error
        .downcast_ref::<crate::error::BridgeError>()
        .expect("the refusal is a typed bridge error");
    assert_eq!(refusal.code, crate::error::BridgeErrorCode::SessionNotOwned);
    assert_eq!(
        refusal.message,
        "Read-only. Message the session that started it."
    );
    let written = |server_requests: &[wiremock::Request]| {
        server_requests
            .iter()
            .filter(|request| {
                String::from_utf8_lossy(&request.body).contains("create_AgentRequest")
            })
            .count()
    };
    assert_eq!(
        written(&server.received_requests().await.unwrap()),
        0,
        "a refused send must not be written"
    );

    core.submit_request("own", owner.did(), "hello", Some("agent"))
        .await?;
    assert_eq!(written(&server.received_requests().await.unwrap()), 1);
    Ok(())
}
