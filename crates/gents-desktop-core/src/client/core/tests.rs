use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use defra_p2p_adapter::{
    ExplicitReplayCapabilityInput, P2PResult, P2pDocumentInfo, P2pDocumentRequest,
    ReplicationFilter, ReplicatorInfo,
};

use super::supervisor::{
    p2p_health_materially_changed, probe_p2p_health, repair_saved_peer,
    request_index_for_ready_peers, saved_peer_needs_repair,
};
use super::writes::cleanup_saved_peer_p2p;
use super::*;
use crate::client::PeerRecord;

#[derive(Default)]
struct RecordingP2P {
    notify_calls: AtomicUsize,
    local_peer_id_error: StdRwLock<Option<String>>,
    listen_addresses: StdRwLock<Vec<String>>,
    listen_addresses_error: StdRwLock<Option<String>>,
    connected_peers: StdRwLock<Vec<String>>,
    connected_peers_error: StdRwLock<Option<String>>,
    connect_calls: StdRwLock<Vec<String>>,
    add_replicator_calls: StdRwLock<Vec<String>>,
    sync_branchable_calls: StdRwLock<Vec<String>>,
    cleanup_calls: StdRwLock<Vec<String>>,
    replicators: StdRwLock<Vec<ReplicatorInfo>>,
    replicators_error: StdRwLock<Option<String>>,
}

#[allow(dead_code)]
impl RecordingP2P {
    fn notify_calls(&self) -> usize {
        self.notify_calls.load(Ordering::SeqCst)
    }

    fn set_local_peer_id_error(&self, error: Option<&str>) {
        *self
            .local_peer_id_error
            .write()
            .expect("local peer id error lock poisoned") = error.map(ToOwned::to_owned);
    }

    fn set_listen_addresses(&self, addrs: Vec<String>) {
        *self
            .listen_addresses
            .write()
            .expect("listen addresses lock poisoned") = addrs;
    }

    fn set_listen_addresses_error(&self, error: Option<&str>) {
        *self
            .listen_addresses_error
            .write()
            .expect("listen addresses error lock poisoned") = error.map(ToOwned::to_owned);
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

    fn add_replicator_calls(&self) -> Vec<String> {
        self.add_replicator_calls
            .read()
            .expect("add replicator calls lock poisoned")
            .clone()
    }

    fn sync_branchable_calls(&self) -> Vec<String> {
        self.sync_branchable_calls
            .read()
            .expect("sync branchable calls lock poisoned")
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

    fn set_replicators_error(&self, error: Option<&str>) {
        *self
            .replicators_error
            .write()
            .expect("replicators error lock poisoned") = error.map(ToOwned::to_owned);
    }
}

#[async_trait]
impl P2POps for RecordingP2P {
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

    async fn add_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
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

    async fn sync_branchable_collection(&self, collection_id: &str) -> P2PResult<()> {
        self.sync_branchable_calls
            .write()
            .expect("sync branchable calls lock poisoned")
            .push(collection_id.to_string());
        Ok(())
    }

    async fn sync_collection_versions(&self, _version_ids: Vec<String>) -> P2PResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn index_sync_request_targets_exactly_the_session_index() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tmp.path().to_path_buf()),
        ClientCoreOptions::local_only(),
    )
    .await
    .expect("client core");
    let recording = Arc::new(RecordingP2P::default());
    recording.set_connected_peers(vec!["peer-alpha".to_string()]);
    let p2p: Arc<dyn P2POps> = recording.clone();

    let requested = super::bootstrap::request_index_sync(core.node(), &p2p)
        .await
        .expect("request index sync");
    assert_eq!(requested, ["AgentConversation", "AgentSession"]);

    let mut expected = crate::client::schema::index_collection_names()
        .into_iter()
        .map(|name| {
            core.node()
                .get_collection(name)
                .expect("get collection")
                .expect("collection exists")
                .collection_id
        })
        .collect::<Vec<_>>();
    let mut actual = recording.sync_branchable_calls();
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected);

    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn supervisor_requests_index_for_new_and_reconnected_peers_and_surfaces_failures() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tmp.path().to_path_buf()),
        ClientCoreOptions::local_only(),
    )
    .await
    .expect("client core");
    let recording = Arc::new(RecordingP2P::default());
    recording.set_connected_peers(vec!["peer-alpha".to_string()]);
    let p2p: Arc<dyn P2POps> = recording.clone();
    let node = core.node_arc();
    let statuses = Arc::new(StdRwLock::new(vec![ClientPeerStatus {
        peer_id: "saved-alpha".to_string(),
        label: "Alpha".to_string(),
        agent_did: "did:key:alpha".to_string(),
        addr: "peer-alpha".to_string(),
        dial_succeeded: true,
        last_error: None,
        pairing: Vec::new(),
        routes: Vec::new(),
        chat_safe: false,
    }]));
    let mut saved = BTreeSet::from(["saved-alpha".to_string()]);
    let mut requested_for = BTreeSet::new();

    request_index_for_ready_peers(&node, &p2p, &statuses, &saved, &mut requested_for).await;
    assert_eq!(recording.sync_branchable_calls().len(), 2);
    assert_eq!(requested_for, saved);

    request_index_for_ready_peers(&node, &p2p, &statuses, &saved, &mut requested_for).await;
    assert_eq!(recording.sync_branchable_calls().len(), 2);

    statuses
        .write()
        .expect("peer statuses lock poisoned")
        .push(ClientPeerStatus {
            peer_id: "saved-beta".to_string(),
            label: "Beta".to_string(),
            agent_did: "did:key:beta".to_string(),
            addr: "peer-beta".to_string(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: false,
        });
    saved.insert("saved-beta".to_string());
    request_index_for_ready_peers(&node, &p2p, &statuses, &saved, &mut requested_for).await;
    assert_eq!(recording.sync_branchable_calls().len(), 4);
    assert_eq!(requested_for, saved);

    requested_for.remove("saved-alpha");
    request_index_for_ready_peers(&node, &p2p, &statuses, &saved, &mut requested_for).await;
    assert_eq!(recording.sync_branchable_calls().len(), 6);

    requested_for.remove("saved-alpha");
    recording.set_connected_peers(Vec::new());
    request_index_for_ready_peers(&node, &p2p, &statuses, &saved, &mut requested_for).await;
    let statuses = statuses.read().expect("peer statuses lock poisoned");
    assert!(statuses
        .iter()
        .find(|status| status.peer_id == "saved-alpha")
        .and_then(|status| status.last_error.as_deref())
        .is_some_and(|error| error.contains("no connected peers")));
    assert!(!requested_for.contains("saved-alpha"));

    core.shutdown().await.expect("shutdown");
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
    core.peer_directory
        .write()
        .await
        .upsert(record.clone())
        .await
        .expect("save invalid peer fixture");
    core.update_peer_status(ClientPeerStatus {
        peer_id: record.peer_id.clone(),
        label: record.label.clone(),
        agent_did: record.agent_did.clone(),
        addr: record.addr.clone(),
        dial_succeeded: true,
        last_error: None,
        pairing: Vec::new(),
        routes: Vec::new(),
        chat_safe: true,
    });
    assert!(core
        .peer_statuses()
        .iter()
        .any(|status| status.peer_id == record.peer_id));
    let route = gents::agent::p2p_reconcile::ClientRouteIdentity::new(
        record.peer_id.clone(),
        "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
        "did:key:desktop-requester",
        record.agent_did.clone(),
    )
    .expect("valid route fixture");
    let manager = super::route_manager::ClientRouteManager::new(
        Arc::clone(&core.node),
        Arc::clone(&core.p2p),
        Arc::new(core.principal.clone()),
    );
    manager
        .upsert_desired(
            &route,
            gents::agent::p2p_reconcile::PairingDirection::RuntimeToClient,
        )
        .await
        .expect("save pairing desired fixture");
    manager
        .upsert_desired(
            &route,
            gents::agent::p2p_reconcile::PairingDirection::RuntimeToClient,
        )
        .await
        .expect("update pairing desired fixture");

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
        .peer_directory
        .read()
        .await
        .pending_removals()
        .iter()
        .any(|pending| pending.peer_id == record.peer_id));
    assert!(!core
        .peer_statuses()
        .iter()
        .any(|status| status.peer_id == record.peer_id));
    let peer_id = gents_protocol::graphql::escape_graphql_string(
        &route.desired_id(gents::agent::p2p_reconcile::PairingDirection::RuntimeToClient),
    );
    let response = core
        .node()
        .execute(&format!(
            r#"query {{ PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{ _docID agent_did profiles collections template replicator_addresses }} }}"#
        ))
        .await;
    assert!(!response.has_errors());
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("PeerPairingDesired"))
        .and_then(|rows| rows.as_array())
        .expect("saved pairing desired rows");
    assert_eq!(rows.len(), 1, "writer must update the existing row");
    let row = rows.first().expect("saved pairing desired row");
    assert_eq!(
        row.get("agent_did").and_then(serde_json::Value::as_str),
        Some("did:test:invalid\"quoted")
    );
    assert!(row.get("profiles").is_some_and(serde_json::Value::is_null));
    assert_eq!(
        row.get("replicator_addresses")
            .and_then(serde_json::Value::as_array)
            .and_then(|addresses| addresses.first())
            .and_then(serde_json::Value::as_str),
        Some(route.transport.address())
    );
    assert!(row
        .get("collections")
        .is_some_and(serde_json::Value::is_null));
    assert_eq!(
        row.get("template").and_then(serde_json::Value::as_str),
        Some("client")
    );

    core.shutdown().await.expect("shutdown");
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
        &p2p,
        &record,
        Some(ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: false,
            last_error: Some("peer Workshop Bay dial failed".to_string()),
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: false,
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
        &p2p,
        &record,
        Some(ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: false,
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
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: false,
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
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: false,
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

    let health = probe_p2p_health(&p2p, &P2PHealth::default()).await;

    assert_eq!(health.status, P2PHealthStatus::Healthy);
    assert_eq!(health.connected_peer_count, 1);
    assert_eq!(health.replicator_count, 1);
    assert_eq!(health.consecutive_failures, 0);
    assert_eq!(health.last_error, None);
    assert!(health.last_ok_at.is_some());
}

#[tokio::test]
async fn probe_p2p_health_marks_repeated_failures_wedged() {
    let recording = Arc::new(RecordingP2P::default());
    recording.set_listen_addresses(vec!["127.0.0.1:56000/p2p/local-peer".to_string()]);
    recording.set_connected_peers_error(Some("channel send error"));
    let p2p: Arc<dyn P2POps> = recording;

    let mut health = P2PHealth::default();
    for _ in 0..P2P_WEDGED_FAILURE_THRESHOLD {
        health = probe_p2p_health(&p2p, &health).await;
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
async fn selected_agent_did_channel_updates_subscribers() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let options = ClientCoreOptions::local_only();
    let core = ClientCore::start_with_paths_and_options(paths, options)
        .await
        .expect("client core");

    let mut rx = core.selected_agent_did_rx();
    assert_eq!(rx.borrow().clone(), None);

    core.set_selected_agent_did(Some("did:alpha".to_string()));
    rx.changed().await.expect("watch update");
    assert_eq!(rx.borrow().clone(), Some("did:alpha".to_string()));

    core.set_selected_agent_did(None);
    rx.changed().await.expect("watch update");
    assert_eq!(rx.borrow().clone(), None);

    core.shutdown().await.expect("shutdown");
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

    core.set_selected_agent_did(Some("did:any".to_string()));
    core.refresh_store().await.expect("refresh scoped");

    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn ensure_agent_loaded_debounces_repeats() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core");

    let first = core.ensure_agent_loaded("did:alpha").await.expect("first");
    let second = core.ensure_agent_loaded("did:alpha").await.expect("second");
    assert!(first, "first call should load");
    assert!(
        !second,
        "second call within debounce window should be a no-op"
    );

    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn ensure_agent_loaded_distinguishes_agents() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core");

    assert!(core.ensure_agent_loaded("did:alpha").await.expect("alpha"));
    assert!(core.ensure_agent_loaded("did:beta").await.expect("beta"));

    core.shutdown().await.expect("shutdown");
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

    core.shutdown().await.expect("shutdown");

    let metrics = core.observer_metrics().await;
    assert!(
        metrics.is_none(),
        "observer metrics should be None after shutdown"
    );
}
