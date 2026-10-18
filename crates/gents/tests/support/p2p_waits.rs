use std::time::{Duration, Instant};

use gents::defra_node::EmbeddedNode;

/// iroh may also advertise a NAT-PMP mapping from the LAN router first
/// (`<router ip>:<port>`), which a local dial through that router times out on.
pub async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = node
            .p2p()
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
        let peers = node
            .p2p()
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
