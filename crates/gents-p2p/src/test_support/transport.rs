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

use std::time::{Duration, Instant};

use defra_node::Node as EmbeddedNode;

// Transport observations run as the Node's own identity, as the runtime's
// P2P owners bind them.

/// iroh may also advertise a NAT-PMP mapping from the LAN router first
/// (`<router ip>:<port>`), which a local dial through that router times out on.
pub async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = gents_store::identity::node_p2p(node)
            .expect("p2p should be enabled")
            .listen_addresses()
            .await
            .expect("listen addresses");
        if let Some(addr) = addrs
            .iter()
            .find(|addr| addr.starts_with("127.0.0.1:") || addr.starts_with("[::1]:"))
        {
            return addr.clone();
        }
        if Instant::now() >= deadline {
            panic!("node never exposed a loopback P2P listen address; last_addrs={addrs:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wait_for_connected_peer(node: &EmbeddedNode) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let peers = gents_store::identity::node_p2p(node)
            .expect("p2p should be enabled")
            .connected_peers()
            .await
            .expect("connected peers");
        if !peers.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("node never reported a connected peer; last_peers={peers:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
