use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use defra_p2p_adapter::{
    ExplicitReplayCapabilityInput, P2PResult, P2pDocumentInfo, P2pDocumentRequest, ReplicatorInfo,
};

use super::bootstrap::{select_local_runtime_pairing_addr, select_runtime_pairing_addr};
use super::supervisor::{
    p2p_health_materially_changed, probe_p2p_health, repair_saved_peer, saved_peer_needs_repair,
};
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
        _collections: Vec<String>,
        _addr: Option<&str>,
    ) -> P2PResult<()> {
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

    async fn republish_document(&self, _collection_name: &str, _doc_id: &str) -> P2PResult<()> {
        Ok(())
    }

    async fn sync_documents(&self, _collection_name: &str, _doc_ids: Vec<String>) -> P2PResult<()> {
        Ok(())
    }

    async fn sync_branchable_collection(&self, _collection_id: &str) -> P2PResult<()> {
        Ok(())
    }

    async fn sync_collection_versions(&self, _version_ids: Vec<String>) -> P2PResult<()> {
        Ok(())
    }
}

#[test]
fn select_local_runtime_pairing_addr_prefers_loopback() {
    let selected = select_local_runtime_pairing_addr(&[
        "100.111.156.102:56000/p2p/peer-alpha".to_string(),
        "127.0.0.1:56000/p2p/peer-alpha".to_string(),
        "peer-alpha".to_string(),
    ]);

    assert_eq!(selected.as_deref(), Some("127.0.0.1:56000/p2p/peer-alpha"));
}

#[test]
fn select_local_runtime_pairing_addr_falls_back_to_first_nonempty() {
    let selected = select_local_runtime_pairing_addr(&[
        "endpointabc123".to_string(),
        "peer-alpha".to_string(),
    ]);

    assert_eq!(selected.as_deref(), Some("endpointabc123"));
}

#[test]
fn select_runtime_pairing_addr_avoids_loopback_for_remote_graphql() {
    let selected = select_runtime_pairing_addr(
        &[
            "127.0.0.1:56000/p2p/peer-alpha".to_string(),
            "100.73.235.38:56000/p2p/peer-alpha".to_string(),
        ],
        "http://100.73.235.38:9181/api/v0/graphql",
    );

    assert_eq!(
        selected.as_deref(),
        Some("100.73.235.38:56000/p2p/peer-alpha")
    );
}

#[tokio::test]
async fn repair_saved_peer_refreshes_network_before_redial() {
    let recording = Arc::new(RecordingP2P::default());
    let p2p: Arc<dyn P2POps> = recording.clone();
    let record = PeerRecord::new(
        "Workshop Bay",
        "127.0.0.1:56000/p2p/peer-alpha",
        "did:defra:workshop-bay",
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
        }),
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
async fn repair_saved_peer_forces_reconfiguration_while_peer_is_connected() {
    let recording = Arc::new(RecordingP2P::default());
    let record = PeerRecord::new(
        "Workshop Bay",
        "127.0.0.1:56000/p2p/peer-alpha",
        "did:defra:workshop-bay",
    );
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
        }),
        true,
        true,
    )
    .await;

    assert_eq!(recording.notify_calls(), 1);
    assert_eq!(recording.connect_calls(), vec![record.addr.clone()]);
    assert_eq!(recording.add_replicator_calls(), vec![record.addr.clone()]);
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
        "did:defra:workshop-bay",
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
        "did:defra:workshop-bay",
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

    // Without selection: should hit load_full_snapshot path.
    core.refresh_store().await.expect("refresh full");

    // With selection: should hit scoped path.
    core.set_selected_agent_did(Some("did:any".to_string()));
    core.refresh_store().await.expect("refresh scoped");

    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn ensure_agent_loaded_debounces_repeats() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core = ClientCore::start_with_paths_and_options(
        paths,
        ClientCoreOptions::local_only(),
    )
    .await
    .expect("core");

    let first = core.ensure_agent_loaded("did:alpha").await.expect("first");
    let second = core.ensure_agent_loaded("did:alpha").await.expect("second");
    assert!(first, "first call should load");
    assert!(!second, "second call within debounce window should be a no-op");

    core.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn ensure_agent_loaded_distinguishes_agents() {
    use crate::client::paths::DesktopPaths;

    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path().to_path_buf());
    let core = ClientCore::start_with_paths_and_options(
        paths,
        ClientCoreOptions::local_only(),
    )
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
