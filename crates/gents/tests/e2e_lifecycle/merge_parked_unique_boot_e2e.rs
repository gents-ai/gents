//! Repro + fence for #984: a P2P merge that parks a unique-index-conflicting
//! document unindexed must not brick the next boot.
//!
//! DefraDB's merge path resolves live unique conflicts deterministically
//! (`db_index::index_manager::save_resolving_unique_conflict`): the
//! lexicographically smallest public docID keeps the index entry and the
//! loser is persisted unindexed. Boot's eager materialization
//! (`ensure_migrations` → `materialize_collection` → `bulk_index`) previously
//! used strict unique saves, so the parked loser re-raised
//! `UniqueConstraintViolation` on every subsequent boot — a foreign stale doc
//! became a permanent poison pill requiring a full home reset.
//!
//! This test reproduces the exact live sequence from 2026-07-31: a re-paired
//! client replays a stale `PeerEndpoint` doc with the same unique `did`, the
//! home merges it gracefully, and the home must then boot again — reporting
//! the parked doc rather than dying on it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::{EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin};
use gents::defra_node::EmbeddedNode;

use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::{first_row, test_p2p_db, DocIdRow};

const CONFLICT_DID: &str = "did:test:984-stale-endpoint";

async fn create_endpoint(node: &EmbeddedNode, address: &str) -> String {
    let mutation = format!(
        r#"mutation {{
            create_PeerEndpoint(input: {{
                did: "{CONFLICT_DID}",
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
    let resp = node
        .execute(&format!(
            r#"{{ PeerEndpoint(filter: {{ did: {{ _eq: "{CONFLICT_DID}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    first_row::<DocIdRow>(&resp, "PeerEndpoint").doc_id
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

/// Unfiltered scan — reads every persisted doc, indexed or parked.
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

    // Same unique `did` on both nodes, different content → distinct docIDs.
    let home_doc = create_endpoint(&home.node, "/ip4/127.0.0.1/tcp/4001").await;
    let client_doc = create_endpoint(&client.node, "/ip4/127.0.0.1/tcp/4002").await;
    assert_ne!(home_doc, client_doc, "conflict requires distinct docIDs");

    // Re-paired client replays its stale endpoint into the home: the merge
    // path parks the deterministic loser unindexed instead of failing.
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

    // The merge parked exactly one doc: the unique index resolves the did to
    // the deterministic winner (smallest docID) while the scan sees both.
    let winner = std::cmp::min(home_doc.clone(), client_doc.clone());
    let parked = std::cmp::max(home_doc.clone(), client_doc.clone());
    let filtered = home
        .node
        .execute(&format!(
            r#"{{ PeerEndpoint(filter: {{ did: {{ _eq: "{CONFLICT_DID}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    let indexed = scan_endpoint_doc_ids(&filtered);
    assert_eq!(
        indexed,
        vec![winner.clone()],
        "unique index must resolve to the deterministic winner"
    );

    // Release the client so the home's restart is the only live node touching
    // the datastore (and to mirror the live incident: server restarts alone).
    client.node.shutdown().await;

    // Boot. Pre-#984-fix this dies inside ensure_migrations with
    // "eager materialization failed: PeerEndpoint: storage error: can not
    // index a doc's field(s) that violates unique index."
    //
    // The reopen is retried on lock errors only: node shutdown does not join
    // the P2P pending-DAG sweep loops (`run_pending_dag_resync`, 60s cadence,
    // spawned untracked in defra-node's `setup_p2p`), so the coordinator —
    // and through it the store — stays alive until the loop's next wake, and
    // the in-process redb lock can lag shutdown by up to one sweep interval.
    // A real process restart has no such lag (the OS drops the lock), so the
    // retry models process exit, not a race in the code under test.
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

    // The boot must be observable about what it tolerated: the report names
    // the parked collection and the parked docID.
    let report = gents::migration::ensure_migrations(&home.node)
        .await
        .expect("ensure_migrations after boot");
    assert_eq!(
        report.materialization.parked_unique_conflicts.len(),
        1,
        "exactly one collection is parked; report={report:?}"
    );
    let detail = &report.materialization.parked_unique_conflicts[0];
    assert!(
        detail.contains("PeerEndpoint"),
        "parked detail must name the collection: {detail}"
    );
    assert!(
        detail.contains(&parked),
        "parked detail must name the parked docID {parked}: {detail}"
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("PeerEndpoint")),
        "report warnings must surface the parked collection: {:?}",
        report.warnings
    );

    // Nothing was lost and the deterministic pick is unchanged: both docs
    // remain persisted, and the unique index still resolves to the winner.
    let mut docs = scan_endpoints(&home.node).await;
    docs.sort();
    let mut expected = vec![home_doc, client_doc];
    expected.sort();
    assert_eq!(docs, expected, "parked doc must survive boot");
    let filtered = home
        .node
        .execute(&format!(
            r#"{{ PeerEndpoint(filter: {{ did: {{ _eq: "{CONFLICT_DID}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert_eq!(
        scan_endpoint_doc_ids(&filtered),
        vec![winner],
        "boot must not flip the deterministic winner"
    );
}
