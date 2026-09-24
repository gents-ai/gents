use super::*;

#[path = "tests/backpressure.rs"]
mod backpressure;
#[path = "tests/overflow.rs"]
mod overflow;

#[test]
fn projection_invalidation_reuses_the_observed_snapshot_allocation() {
    let (store, _) = ObservedStore::new(ClientStore::default());
    let held = store.snapshot();
    let pointer = Arc::as_ptr(&held);
    let initial = store.projection_revision();
    store.invalidate_projection();
    assert_eq!(Arc::as_ptr(&store.snapshot()), pointer);
    assert_eq!(
        store.projection_revision().store_version,
        initial.store_version + 1
    );
    assert_eq!(
        store.projection_revision().reconcile_version,
        initial.reconcile_version + 1
    );
}

#[test]
fn observer_merge_advances_both_revision_fences_and_strips_transcript_rows() {
    let (store, _) = ObservedStore::new(ClientStore::default());
    let initial = store.projection_revision();
    store.merge_observer_patch(ClientStore::default());
    let current = store.projection_revision();
    assert_eq!(current.store_version, initial.store_version + 1);
    assert_eq!(current.reconcile_version, initial.reconcile_version + 1);
    assert!(store.snapshot().transcript_messages.is_empty());
    assert!(store.snapshot().output_segments.is_empty());
}

use crate::client::schema::ensure_runtime_schemas;
use defra_node::{EventName, NodeBuilder};
use std::sync::Arc;

async fn build_observer_fixture() -> (
    tempfile::TempDir,
    Arc<EmbeddedNode>,
    Arc<ObservedStore>,
    ObserverHandle,
) {
    let tempdir = tempfile::tempdir().expect("observer peer directory");
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let (store, _rx) = ObservedStore::new(crate::client::store::ClientStore::default());
    let peer_dir = crate::client::peer_directory::PeerDirectory::open_writer(
        tempdir.path().join("peers.json"),
    )
    .await
    .expect("peer_directory");
    let configured_peers = crate::client::core::sync_state::ClientSyncStateOwner::new(
        crate::client::core::P2PHealth::default(),
        peer_dir,
        Vec::new(),
    );
    let subscription = node.subscribe_document_changes();
    let (_tx, rx) = watch::channel::<Option<String>>(None);
    let handle = spawn_observer_with_selection(
        node.clone(),
        store.clone(),
        configured_peers,
        "did:test:requester".to_string(),
        subscription,
        rx,
    );
    (tempdir, node, store, handle)
}

async fn seed_principal(node: &EmbeddedNode, did: &str) {
    let mutation = format!(
        r#"mutation {{
                create_AgentPrincipal(input: {{
                    agent_did: "{did}",
                    display_name: "{did}",
                    default_behavior_id: "default",
                    enabled: true,
                    created_at: "2026-05-07T00:00:00Z",
                    created_by: "test"
                }}) {{ _docID }}
            }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

async fn seed_message(node: &EmbeddedNode, session_id: &str, seq: i64, content: &str) {
    let mutation = format!(
        r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{session_id}:{seq}",
                    session_id: "{session_id}",
                    agent_did: "did:test:selected",
                    publication: {{kind: "fork", origin_message_doc_id: "{session_id}:source"}},
                    outcome: "complete",
                    sequence: {seq},
                    role: "user",
                    native_id: "{content}",
                    blocks: [],
                    created_at: "2026-05-07T00:00:00Z"
                }}) {{ _docID }}
            }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

async fn seed_output_segment(node: &EmbeddedNode, ordinal: i64) {
    let mutation = format!(
        r#"mutation {{
            create_AgentOutputSegment(input: {{
                agent_did: "did:alpha",
                session_id: "sess-1",
                request_doc_id: "request-physical",
                source: {{kind: "authored", key: "burst"}},
                ordinal: {ordinal},
                writer: {{kind: "request_execution", execution_generation: "generation-1"}},
                runs: [{{stream: 0, bytes: 1, declaration: {{block_index: 0, part_index: 0, payload: {{kind: "text"}}}}}}],
                payload: "x",
                created_at: "2026-05-07T00:00:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

#[tokio::test]
async fn streaming_commits_converge_without_a_forced_batch_window() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;

    let metrics_before = handle.metrics_snapshot();
    for ordinal in 0..50 {
        seed_output_segment(node.as_ref(), ordinal).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let metrics_after = handle.metrics_snapshot();

    let fetches = metrics_after.docs_fetched - metrics_before.docs_fetched;
    let flushes = metrics_after.debounce_flushes - metrics_before.debounce_flushes;
    assert!(
        fetches <= 51,
        "more fetches than committed documents: {fetches}"
    );
    assert!(
        flushes >= 1 && flushes <= 51,
        "expected 1..=51 flushes, got {flushes}"
    );

    assert!(store.snapshot().output_segments.is_empty());
    assert!(metrics_after.transcript_invalidations >= 1);

    handle.shutdown().await;
}

#[tokio::test]
async fn multi_collection_burst_fans_out_correctly() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    seed_principal(node.as_ref(), "did:alpha").await;

    for i in 1..=5 {
        let update_resp = format!(
            r#"mutation {{ update_AgentPrincipal(filter: {{ agent_did: {{ _eq: "did:alpha" }} }}, input: {{ display_name: "alpha-{i}" }}) {{ _docID }} }}"#
        );
        node.execute(&update_resp).await;

        seed_message(node.as_ref(), "sess-1", i, &format!("msg-{i}")).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let snap = store.snapshot();

    assert_eq!(
        snap.agent_principals
            .iter()
            .find(|row| row.agent_did == "did:alpha")
            .and_then(|row| row.display_name.as_deref()),
        Some("alpha-5")
    );

    assert!(
        snap.transcript_messages.is_empty(),
        "observer retained transcript rows"
    );
    assert!(handle.metrics_snapshot().transcript_invalidations >= 1);

    handle.shutdown().await;
}

#[tokio::test]
async fn dropped_events_with_no_selection_falls_back_to_full() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    seed_principal(node.as_ref(), "did:zero").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let snap = store.snapshot();
    assert!(
        snap.agent_principals
            .iter()
            .any(|p| p.agent_did == "did:zero"),
        "expected did:zero in store"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn transcript_create_and_delete_only_invalidate_the_projection() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;

    let before = store.projection_revision();
    seed_message(node.as_ref(), "sess-1", 1, "before-delete").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(store.snapshot().transcript_messages.is_empty());
    let after_create = store.projection_revision();
    assert!(after_create.reconcile_version > before.reconcile_version);

    node.execute(
            r#"mutation { delete_AgentMessage(filter: { message_key: { _eq: "sess-1:1" } }) { _docID } }"#,
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(store.snapshot().transcript_messages.is_empty());
    assert!(store.projection_revision().reconcile_version > after_create.reconcile_version);
    assert!(handle.metrics_snapshot().transcript_invalidations >= 2);
    handle.shutdown().await;
}

#[tokio::test]
async fn hydration_control_updates_only_invalidate_the_session_projection() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let mut changes = store.subscribe_changes();
    let before = store.projection_revision();

    let create = node
        .execute(
            r#"mutation {
                create_SessionHydrationRequest(input: {
                    request_key: "peer-1:session-1"
                    requester_did: "did:test:requester"
                    agent_did: "did:test:agent"
                    session_id: "session-1"
                    created_at: "2026-08-28T00:00:00Z"
                    status: "pending"
                    status_detail: ""
                    served_doc_count: 0
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!create.has_errors(), "{:?}", create.errors);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let after_create = store.projection_revision();
    assert!(after_create.reconcile_version > before.reconcile_version);
    assert_eq!(changes.borrow_and_update().revision, after_create);
    assert_eq!(store.snapshot().row_count(), 0);

    let update = node
        .execute(
            r#"mutation {
                update_SessionHydrationRequest(
                    filter: { request_key: { _eq: "peer-1:session-1" } }
                    input: { status: "served", served_doc_count: 0 }
                ) { _docID }
            }"#,
        )
        .await;
    assert!(!update.has_errors(), "{:?}", update.errors);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(store.projection_revision().reconcile_version > after_create.reconcile_version);
    assert_eq!(
        changes.borrow_and_update().revision,
        store.projection_revision()
    );
    assert_eq!(store.snapshot().row_count(), 0);
    handle.shutdown().await;
}

#[tokio::test]
async fn fetch_failures_increment_on_unknown_collection() {
    let (_tempdir, node, _store, handle) = build_observer_fixture().await;

    let result =
        crate::client::query::fetch_doc_patch(node.as_ref(), "NotARealCollection", &["x"]).await;
    assert!(result.is_err(), "expected error for unknown collection");

    let snap = handle.metrics_snapshot();
    assert_eq!(snap.fetch_failures, 0);
    handle.shutdown().await;
}

#[tokio::test]
async fn local_write_increments_redundant_fetch_counter() {
    let (_tempdir, node, _store, handle) = build_observer_fixture().await;

    seed_message(node.as_ref(), "sess-2", 1, "local").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let snap = handle.metrics_snapshot();
    assert!(
        snap.local_write_redundant_fetches >= 1,
        "expected at least 1 local-write fetch; got {}",
        snap.local_write_redundant_fetches
    );
    handle.shutdown().await;
}
