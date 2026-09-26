use std::time::{Duration, Instant};

use gents::defra_node::EmbeddedNode;

async fn local_peer_id(node: &EmbeddedNode) -> Option<String> {
    node.p2p()?.local_peer_id().await.ok()
}

pub async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    wait_for_listen_addr_at(node, "P2P setup").await
}

/// `boundary` leads every panic this wait raises, so a failure names the
/// caller's step without relying on log capture or subscriber filters.
pub async fn wait_for_listen_addr_at(node: &EmbeddedNode, boundary: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = node
            .p2p()
            .unwrap_or_else(|| panic!("{boundary}: p2p should be enabled"))
            .listen_addresses()
            .await
            .unwrap_or_else(|error| panic!("{boundary}: listen addresses: {error:?}"));
        if let Some(addr) = addrs.first() {
            return addr.clone();
        }
        if Instant::now() >= deadline {
            panic!(
                "{boundary}: node {:?} never exposed a P2P listen address; last_addrs={addrs:?}",
                local_peer_id(node).await
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wait_for_connected_peer(node: &EmbeddedNode) {
    wait_for_connected_peer_at(node, "P2P setup").await
}

/// `boundary` leads every panic this wait raises, so a failure names the
/// caller's step without relying on log capture or subscriber filters.
pub async fn wait_for_connected_peer_at(node: &EmbeddedNode, boundary: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let peers = node
            .p2p()
            .unwrap_or_else(|| panic!("{boundary}: p2p should be enabled"))
            .connected_peers()
            .await
            .unwrap_or_else(|error| panic!("{boundary}: connected peers: {error:?}"));
        if !peers.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "{boundary}: node {:?} never reported a connected peer; last_peers={peers:?}",
                local_peer_id(node).await
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
