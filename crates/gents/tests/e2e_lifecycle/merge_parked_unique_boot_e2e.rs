//! External premises: DefraDB's merge path keeps the unique-index entry for
//! the lexicographically smallest docID and persists the loser unindexed.

use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use gents::defra_node::Node as EmbeddedNode;
use gents_p2p::p2p_reconcile::{EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin};
use gents_store::graphql::escape_graphql_string;

use crate::support::snapshots::SnapshotReader;
use crate::support::test_p2p_db;
use gents_p2p::test_support::transport::{wait_for_connected_peer, wait_for_listen_addr};

const CONFLICT_DID: &str = "did:test:984-stale-endpoint";

async fn create_endpoint(node: &std::sync::Arc<EmbeddedNode>, address: &str) -> String {
    let did = escape_graphql_string(CONFLICT_DID);
    let address = escape_graphql_string(address);
    let mutation = format!(
        r#"mutation {{
            create_PeerRegistry(input: {{
                peer_id: "{did}",
                node_did: "did:test:node",
                addresses: ["{address}"],
                endpoint_address: "{address}",
                endpoint_updated_at: "2026-07-31T00:00:00Z",
                binding_sig: "sig-{address}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = gents_store::config_client::ConfigAccess::write_local_response(
        node,
        "test.p2p_fixture",
        &mutation,
    )
    .await
    .expect("P2P document access");
    assert!(
        !resp.has_errors(),
        "create_PeerRegistry failed: {:?}",
        resp.errors
    );
    let indexed = indexed_endpoint_doc_ids(node).await;
    assert_eq!(indexed.len(), 1, "freshly created endpoint must be indexed");
    indexed.into_iter().next().unwrap()
}

async fn indexed_endpoint_doc_ids(node: &std::sync::Arc<EmbeddedNode>) -> Vec<String> {
    let did = escape_graphql_string(CONFLICT_DID);
    let resp = gents_store::graphql::graphql_with_transaction_retry(
        node,
        &format!(r#"{{ PeerRegistry(filter: {{ peer_id: {{ _eq: "{did}" }} }}) {{ _docID }} }}"#),
        "test.p2p_observation",
    )
    .await
    .expect("P2P document access");
    scan_endpoint_doc_ids(&resp)
}

fn scan_endpoint_doc_ids(resp: &gents::defra_node::QueryResponse) -> Vec<String> {
    assert!(!resp.has_errors(), "scan failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|d| d.get("PeerRegistry"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("_docID").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn scan_endpoints(node: &std::sync::Arc<EmbeddedNode>) -> Vec<String> {
    let resp = gents_store::graphql::graphql_with_transaction_retry(
        node,
        r#"{ PeerRegistry { _docID peer_id endpoint_address } }"#,
        "test.p2p_observation",
    )
    .await
    .expect("P2P document access");
    scan_endpoint_doc_ids(&resp)
}

async fn install_replicator(sender: &Arc<EmbeddedNode>, receiver: &Arc<EmbeddedNode>) {
    let receiver_addr = wait_for_listen_addr(receiver).await;
    let sender_addr = wait_for_listen_addr(sender).await;
    let collections = vec!["PeerRegistry".to_string()];

    // Transport administration runs as each Node's own identity.
    let sender_p2p = gents_store::identity::node_p2p(sender).expect("sender p2p");
    let receiver_p2p = gents_store::identity::node_p2p(receiver).expect("receiver p2p");
    sender_p2p
        .connect_peer(&receiver_addr)
        .await
        .expect("connect sender to receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    sender_p2p
        .add_collections(collections.clone())
        .await
        .expect("sender p2p collections");
    receiver_p2p
        .add_collections(collections.clone())
        .await
        .expect("receiver p2p collections");
    receiver_p2p
        .add_replicator(
            collections.clone(),
            Some(&sender_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("authorize sender as receiver-side replicator");

    let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(sender));
    sender_admin
        .add_replicator(&[receiver_addr], &collections, &PairingFilters::new())
        .await
        .expect("install sender to receiver replicator");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn merge_parked_unique_conflict_does_not_brick_boot() {
    let mut home = test_p2p_db("984-home").await;
    let client = test_p2p_db("984-client").await;

    let home_doc = create_endpoint(&home.node, "/ip4/127.0.0.1/tcp/4001").await;
    let client_doc = create_endpoint(&client.node, "/ip4/127.0.0.1/tcp/4002").await;
    assert_ne!(home_doc, client_doc, "conflict requires distinct docIDs");

    install_replicator(&client.node, &home.node).await;

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut changes = home.node.subscribe_changes("PeerRegistry").await;
    loop {
        let docs = scan_endpoints(&home.node).await;
        if docs.len() == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "home never persisted the conflicting endpoint; scan={docs:?}"
        );
        tokio::time::timeout_at(deadline, changes.recv())
            .await
            .expect("conflicting endpoint notification deadline");
    }

    let winner = std::cmp::min(home_doc.clone(), client_doc.clone());
    assert_eq!(
        indexed_endpoint_doc_ids(&home.node).await,
        vec![winner.clone()],
        "unique index must resolve to the deterministic winner"
    );

    client.node.shutdown().await.expect("node shutdown");

    drop(changes);
    tokio::time::timeout(Duration::from_secs(90), home.node.shutdown())
        .await
        .expect("node shutdown deadline")
        .expect("node shutdown");
    tokio::time::timeout(Duration::from_secs(90), home.simulate_process_crash())
        .await
        .expect("reopen deadline")
        .expect("boot after a merge-parked unique-index conflict");

    gents_store::schema::migration::ensure_migrations(&home.node)
        .await
        .expect("ensure_migrations after boot");

    let mut docs = scan_endpoints(&home.node).await;
    docs.sort();
    let mut expected = vec![home_doc, client_doc];
    expected.sort();
    assert_eq!(docs, expected, "parked doc must survive boot");
    assert_eq!(
        indexed_endpoint_doc_ids(&home.node).await,
        vec![winner],
        "boot must not flip the deterministic winner"
    );
}
