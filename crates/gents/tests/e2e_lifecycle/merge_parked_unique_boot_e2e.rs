//! External premises: DefraDB's merge path keeps the unique-index entry for
//! the lexicographically smallest docID and persists the loser unindexed.
//! Node shutdown does not join the P2P pending-DAG sweep loops spawned in
//! defra-node's `setup_p2p`, so
//! the in-process storage lock can lag shutdown by up to one sweep interval; the
//! OS drops the lock on real process exit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::{EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;

use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::test_p2p_db;

const CONFLICT_DID: &str = "did:test:984-stale-endpoint";

async fn create_endpoint(node: &EmbeddedNode, address: &str) -> String {
    let did = escape_graphql_string(CONFLICT_DID);
    let address = escape_graphql_string(address);
    let mutation = format!(
        r#"mutation {{
            create_PeerEndpoint(input: {{
                did: "{did}",
                node_id: "node-{address}",
                address: "{address}",
                updated_at: "2026-07-31T00:00:00Z",
                binding_sig: "sig-{address}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_PeerEndpoint failed: {:?}",
        resp.errors
    );
    let indexed = indexed_endpoint_doc_ids(node).await;
    assert_eq!(indexed.len(), 1, "freshly created endpoint must be indexed");
    indexed.into_iter().next().unwrap()
}

async fn indexed_endpoint_doc_ids(node: &EmbeddedNode) -> Vec<String> {
    let did = escape_graphql_string(CONFLICT_DID);
    let resp = node
        .execute(&format!(
            r#"{{ PeerEndpoint(filter: {{ did: {{ _eq: "{did}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    scan_endpoint_doc_ids(&resp)
}

fn scan_endpoint_doc_ids(resp: &gents::defra_node::QueryResponse) -> Vec<String> {
    assert!(!resp.has_errors(), "scan failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|d| d.get("PeerEndpoint"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("_docID").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn scan_endpoints(node: &EmbeddedNode) -> Vec<String> {
    let resp = node
        .execute(r#"{ PeerEndpoint { _docID did address } }"#)
        .await;
    scan_endpoint_doc_ids(&resp)
}

async fn install_replicator(sender: &Arc<EmbeddedNode>, receiver: &Arc<EmbeddedNode>) {
    let receiver_addr = wait_for_listen_addr(receiver).await;
    let sender_addr = wait_for_listen_addr(sender).await;
    let collections = vec!["PeerEndpoint".to_string()];

    sender
        .p2p()
        .expect("sender p2p")
        .connect_peer(&receiver_addr)
        .await
        .expect("connect sender to receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    sender
        .p2p()
        .expect("sender p2p")
        .add_collections(collections.clone())
        .await
        .expect("sender p2p collections");
    receiver
        .p2p()
        .expect("receiver p2p")
        .add_collections(collections.clone())
        .await
        .expect("receiver p2p collections");
    receiver
        .p2p()
        .expect("receiver p2p")
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
    loop {
        let docs = scan_endpoints(&home.node).await;
        if docs.len() == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "home never persisted the conflicting endpoint; scan={docs:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let winner = std::cmp::min(home_doc.clone(), client_doc.clone());
    assert_eq!(
        indexed_endpoint_doc_ids(&home.node).await,
        vec![winner.clone()],
        "unique index must resolve to the deterministic winner"
    );

    client.node.shutdown().await;

    let reopen_deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match home.simulate_process_crash().await {
            Ok(()) => break,
            Err(e)
                if e.to_string().contains("is locked by another process")
                    && Instant::now() < reopen_deadline =>
            {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => panic!("boot after a merge-parked unique-index conflict must succeed: {e}"),
        }
    }

    gents::store::schema::migration::ensure_migrations(&home.node)
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
