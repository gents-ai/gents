use defra_node::P2PConfig;

#[derive(Debug, Clone)]
pub struct TestP2pAdmission {
    pub max_concurrent_push_tasks: usize,
    pub max_concurrent_dag_fetches: usize,
    pub max_pending_dags: usize,
    pub rate_limit_burst: u32,
    pub rate_limit_rate: f64,
}

impl Default for TestP2pAdmission {
    fn default() -> Self {
        Self {
            max_concurrent_push_tasks: p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS,
            max_concurrent_dag_fetches: p2p::sync::DEFAULT_MAX_CONCURRENT_DAG_FETCHES,
            max_pending_dags: p2p::sync::DEFAULT_MAX_PENDING_DAGS,
            rate_limit_burst: p2p::sync::DEFAULT_RATE_LIMIT_BURST,
            rate_limit_rate: p2p::sync::DEFAULT_RATE_LIMIT_RATE,
        }
    }
}

impl TestP2pAdmission {
    pub fn single_push_worker() -> Self {
        Self {
            max_concurrent_push_tasks: 1,
            ..Self::default()
        }
    }
}

pub fn test_p2p_config(admission: &TestP2pAdmission, data_path: &std::path::Path) -> P2PConfig {
    P2PConfig {
        port: Some(0),
        bind_addr: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        relay_mode: p2p::iroh::IrohRelayModeConfig::Disabled,
        discovery: p2p::iroh::IrohDiscoveryConfig::Disabled,
        allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
        max_concurrent_multipath_paths: None,
        secret_key_path: Some(data_path.join("p2p.key")),
        peer_key: defra_node::PeerKeySource::File,
        load_persisted_collections: true,
        sync: defra_node::SyncTuning {
            max_concurrent_dag_fetches: admission.max_concurrent_dag_fetches,
            max_concurrent_push_tasks: admission.max_concurrent_push_tasks,
            rate_limit_burst: admission.rate_limit_burst,
            rate_limit_rate: admission.rate_limit_rate,
            max_doc_sync_request_doc_ids: p2p::sync::DEFAULT_MAX_DOC_SYNC_REQUEST_DOC_IDS,
            max_pending_dags: admission.max_pending_dags,
            ..Default::default()
        },
        rebroadcast_on_merge: false,
    }
}

use std::time::Duration;

use defra_node::Node as EmbeddedNode;

pub async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    let p2p = gents_store::identity::node_p2p(node).expect("P2P enabled");
    let addresses = tokio::time::timeout(Duration::from_secs(10), p2p.listen_addresses())
        .await
        .expect("P2P listen-address deadline")
        .expect("P2P listen addresses");
    addresses
        .into_iter()
        .find(|address| address.starts_with("127.0.0.1:") || address.starts_with("[::1]:"))
        .expect("the loopback P2P endpoint must be bound when the node is created")
}

pub async fn wait_for_connected_peer(node: &EmbeddedNode) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut changes = node.subscribe(&[defra_node::EventName::PeerConnection]);
    loop {
        let peers = gents_store::identity::node_p2p(node)
            .expect("P2P enabled")
            .connected_peers()
            .await
            .expect("connected peers");
        if !peers.is_empty() {
            return;
        }
        tokio::time::timeout_at(deadline, changes.recv())
            .await
            .expect("no connected peer before the connection deadline")
            .expect("peer connection subscription closed");
    }
}
