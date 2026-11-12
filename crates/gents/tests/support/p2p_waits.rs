use std::time::Duration;

use gents::defra_node::Node as EmbeddedNode;

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
    let mut changes = node.subscribe(&[gents::defra_node::EventName::PeerConnection]);
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
