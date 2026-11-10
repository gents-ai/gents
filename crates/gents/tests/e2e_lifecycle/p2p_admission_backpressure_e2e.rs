use std::time::{Duration, Instant};

use gents::defra_node::Node as EmbeddedNode;
use gents::store::graphql::escape_graphql_string;

use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::{test_p2p_db_with_admission, TestP2pAdmission};

const OWNER_DID: &str = "did:test:admission-p2p-owner";

async fn install_one_way_replicator(
    sender: &EmbeddedNode,
    receiver: &EmbeddedNode,
    collections: &[&str],
) {
    let sender_addr = wait_for_listen_addr(sender).await;
    let receiver_addr = wait_for_listen_addr(receiver).await;
    // Transport administration runs as each Node's own identity.
    let sender_p2p = gents::store::identity::node_p2p(sender).expect("sender p2p");
    let receiver_p2p = gents::store::identity::node_p2p(receiver).expect("receiver p2p");

    sender_p2p
        .connect_peer(&receiver_addr)
        .await
        .expect("connect sender to receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    let collection_names = collections
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    sender_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add sender p2p collections");
    receiver_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add receiver p2p collections");
    receiver_p2p
        .add_replicator(
            collection_names.clone(),
            Some(&sender_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("authorize sender as receiver-side replicator");
    sender_p2p
        .add_replicator(
            collection_names,
            Some(&receiver_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install sender to receiver replicator");
}

/// Publishes one discovery record, the only document class pairing carries.
async fn publish_peer_record(node: &EmbeddedNode, peer_id: &str, address: &str) {
    let peer_id = escape_graphql_string(peer_id);
    let address = escape_graphql_string(address);
    let mutation = format!(
        r#"mutation {{
            create_PeerRegistry(input: {{
                peer_id: "{peer_id}",
                node_did: "{OWNER_DID}",
                addresses: ["{address}"],
                endpoint_address: "{address}",
                endpoint_updated_at: "2026-07-09T00:00:00Z",
                binding_sig: "sig-{address}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create PeerRegistry failed: {:?}",
        resp.errors
    );
}

async fn wait_for_peer_record(
    node: &EmbeddedNode,
    peer_id: &str,
    timeout: Duration,
    label: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    let query = format!(
        r#"{{ PeerRegistry(filter: {{ peer_id: {{ _eq: "{}" }} }}, limit: 1) {{ peer_id node_did endpoint_address }} }}"#,
        escape_graphql_string(peer_id)
    );
    loop {
        let resp = node.execute(&query).await;
        if let Some(row) = resp
            .data
            .as_ref()
            .and_then(|data| data["PeerRegistry"].as_array())
            .and_then(|rows| rows.first())
        {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for PeerRegistry({peer_id}) on {label}: {:?}",
            resp.errors
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_push_worker_delivers_multi_wave_updates() {
    let admission = TestP2pAdmission::single_push_worker();
    let owner = test_p2p_db_with_admission("admission-p2p-multi-owner", admission.clone()).await;
    let peer = test_p2p_db_with_admission("admission-p2p-multi-peer", admission).await;

    install_one_way_replicator(owner.node.as_ref(), peer.node.as_ref(), &["PeerRegistry"]).await;

    for idx in 0..3 {
        let peer_id = format!("admission-p2p-multi-wave-{idx}");
        let address = format!("/ip4/127.0.0.1/tcp/{}", 4100 + idx);
        publish_peer_record(owner.node.as_ref(), &peer_id, &address).await;
        let on_peer = wait_for_peer_record(
            peer.node.as_ref(),
            &peer_id,
            Duration::from_secs(45),
            &format!("peer wave {idx}"),
        )
        .await;
        assert_eq!(on_peer["node_did"], OWNER_DID);
        assert_eq!(on_peer["endpoint_address"], address.as_str());
    }
}
