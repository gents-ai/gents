use super::*;
use crate::client::store::ClientStoreRows;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::schemas::AGENT_REQUEST_NAME;

async fn publish_request_invalidation(node: &EmbeddedNode, doc_id: &str) {
    let collection_id = node
        .get_collection(AGENT_REQUEST_NAME)
        .expect("get AgentRequest collection")
        .expect("AgentRequest schema installed")
        .collection_id;
    let block = b"observer missing-request invalidation";
    let cid = "QmYwAPJzv5CZsnAzt8auVZRnGi2C6ihTNdEWbj5Qigx7Ha"
        .parse()
        .expect("test invalidation CID");
    node.event_bus()
        .publish(events::Message::update(events::Update::new(
            doc_id.to_owned(),
            cid,
            collection_id,
            block.to_vec(),
            false,
            true,
        )));
}

async fn wait_for_node_projection(store: &ObservedStore, node_did: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .snapshot()
                .nodes
                .iter()
                .any(|row| row.node_did == node_did)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("observer should process the later Node change");
}

async fn wait_for_node_and_request_absence(
    store: &ObservedStore,
    node_did: &str,
    missing_doc_id: &str,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = store.snapshot();
            let node_loaded = snapshot.nodes.iter().any(|row| row.node_did == node_did);
            let request_removed = !snapshot
                .requests
                .iter()
                .any(|row| row.doc_id.as_deref() == Some(missing_doc_id));
            if node_loaded && request_removed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("observer should process the later Node change and request deletion");
}

#[tokio::test]
async fn collection_head_notifications_do_not_schedule_missing_document_reloads() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let metrics = Arc::clone(&handle.metrics);
    seed_node(node.as_ref(), "did:test:document-with-collection-head").await;
    wait_for_node_projection(&store, "did:test:document-with-collection-head").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while metrics.snapshot().events_received < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("branchable write publishes document and collection-head notifications");
    handle.shutdown().await;
    let metrics = metrics.snapshot();
    assert_eq!(metrics.scope_reloads, 0);
    assert_eq!(metrics.fetch_failures, 0);
    assert_eq!(metrics.docs_fetched, 1);
}

#[tokio::test]
async fn an_uncached_missing_request_does_not_reload_the_projection() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let before = handle.metrics_snapshot();
    let revision_before = store.projection_revision();
    let mut store_version = store.subscribe();
    let public_version_before = *store_version.borrow_and_update();

    publish_request_invalidation(node.as_ref(), "bae-37b10157-24d3-559b-8646-b82dedbdfb35").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while store.projection_revision().request_event_version
            == revision_before.request_event_version
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request event should advance the internal publication fence");
    assert_eq!(*store_version.borrow(), public_version_before);
    assert!(!store_version
        .has_changed()
        .expect("store version channel open"));
    seed_node(node.as_ref(), "did:test:observer-after-missing").await;
    wait_for_node_projection(&store, "did:test:observer-after-missing").await;

    let after = handle.metrics_snapshot();
    assert_eq!(after.scope_reloads, before.scope_reloads);
    let revision_after = store.projection_revision();
    assert!(revision_after.request_event_version > revision_before.request_event_version);
    assert_eq!(
        revision_after.provenance_version,
        revision_before.provenance_version
    );
    assert!(store.snapshot().requests.is_empty());
    assert!(after.docs_fetched > before.docs_fetched);
    handle.shutdown().await;
}

#[tokio::test]
async fn scoped_resync_request_reloads_that_node_through_the_observer_owner() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let node_did = "did:test:requested-resync-scope";
    seed_node(node.as_ref(), node_did).await;
    wait_for_node_projection(&store, node_did).await;
    let before = handle.metrics_snapshot().scope_reloads;

    store.replace_snapshot(crate::client::store::ClientStore::default());
    store.request_resync(Some(node_did));
    store.request_resync(Some(node_did));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .snapshot()
                .nodes
                .iter()
                .any(|row| row.node_did == node_did)
                && handle.metrics_snapshot().scope_reloads > before
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("coalesced scoped resync should reach the existing observer loader");
    assert_eq!(
        handle.metrics_snapshot().scope_reloads - before,
        1,
        "duplicate selected-refresh recovery requests should coalesce"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn a_missing_cached_request_still_reloads_and_removes_the_request() {
    let (_tempdir, node, store, handle) =
        build_observer_fixture_for_selection(Some("did:test:selected".to_owned())).await;
    let missing_doc_id = "bae-7ae82f02-e54f-5f76-bbd5-f03cf03d0951";
    let cached = AgentRequestRow {
        doc_id: Some(missing_doc_id.to_owned()),
        request_id: "cached-request".to_owned(),
        node_did: Some("did:test:cached-request".to_owned()),
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        ..AgentRequestRow::default()
    };
    store.merge_observer_patch(crate::client::store::ClientStore::from_rows(
        ClientStoreRows {
            requests: vec![cached],
            ..ClientStoreRows::default()
        },
    ));
    assert!(store
        .snapshot()
        .requests
        .iter()
        .any(|row| row.doc_id.as_deref() == Some(missing_doc_id)));

    publish_request_invalidation(node.as_ref(), missing_doc_id).await;
    seed_node(node.as_ref(), "did:test:observer-after-delete").await;
    wait_for_node_and_request_absence(&store, "did:test:observer-after-delete", missing_doc_id)
        .await;

    assert!(handle.metrics_snapshot().scope_reloads >= 1);
    assert!(!store
        .snapshot()
        .requests
        .iter()
        .any(|row| row.doc_id.as_deref() == Some(missing_doc_id)));
    handle.shutdown().await;
}
