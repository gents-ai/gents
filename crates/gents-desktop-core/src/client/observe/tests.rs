use super::*;

fn response_patch(content: &str, progress_seq: i64) -> ClientStore {
    ClientStore::from_rows(crate::client::store::ClientStoreRows {
        responses: vec![serde_json::from_value(serde_json::json!({
            "response_key": "response-1",
            "request_id": "request-1",
            "agent_did": "did:agent:1",
            "session_id": "session-1",
            "content": content,
            "status": "streaming",
            "progress_seq": progress_seq
        }))
        .expect("response row")],
        ..crate::client::store::ClientStoreRows::default()
    })
}

#[test]
fn response_only_updates_advance_store_without_advancing_reconcile_revision() {
    let (store, _) = ObservedStore::new(ClientStore::default());
    let mut changes = store.subscribe_changes();
    let initial = store.projection_revision();

    // A bearer isolation filter may truthfully strip a response patch to zero
    // rows. It is still response-only work and must not break live-delta
    // continuity by advancing the structural reconcile fence.
    store.merge_observer_patch(ClientStore::default(), true);
    let response_notice = *changes.borrow_and_update();
    assert!(response_notice.response_only);
    assert_eq!(
        response_notice.revision.store_version,
        initial.store_version + 1
    );
    assert_eq!(
        response_notice.revision.reconcile_version,
        initial.reconcile_version
    );

    store.replace_snapshot(ClientStore::default());
    let reconcile_notice = *changes.borrow_and_update();
    assert!(!reconcile_notice.response_only);
    assert_eq!(
        reconcile_notice.revision.reconcile_version,
        initial.reconcile_version + 1
    );
}

#[test]
fn projection_invalidation_reuses_the_observed_snapshot_allocation() {
    let (store, _) = ObservedStore::new(response_patch("a", 1));
    let held = store.snapshot();
    let pointer = Arc::as_ptr(&held);

    store.invalidate_projection();

    let current = store.snapshot();
    assert_eq!(Arc::as_ptr(&current), pointer);
}

#[test]
fn response_only_merge_is_in_place_unless_a_reader_needs_snapshot_isolation() {
    let initial = ClientStore::from_rows(crate::client::store::ClientStoreRows {
        responses: response_patch("a", 1).responses,
        messages: (0..600)
            .map(|sequence| {
                serde_json::from_value(serde_json::json!({
                    "message_key": format!("session-1:{sequence}"),
                    "session_id": "session-1",
                    "sequence": sequence,
                    "role": "assistant",
                    "content": format!("durable row {sequence}")
                }))
                .expect("message row")
            })
            .collect(),
        ..crate::client::store::ClientStoreRows::default()
    });
    let (store, _) = ObservedStore::new(initial);

    for progress_seq in 2..=51 {
        let outcome = store.merge_observer_patch_with_outcome(
            response_patch(&"x".repeat(progress_seq as usize), progress_seq),
            true,
        );
        assert!(outcome.response_only);
        assert!(!outcome.copied_snapshot);
    }
    assert!(store.snapshot().messages.is_empty());

    let expected_held_content = "x".repeat(51);
    let held_reader = store.snapshot();
    let second = store.merge_observer_patch_with_outcome(response_patch("terminal", 52), true);
    assert!(second.response_only);
    assert!(second.copied_snapshot);
    assert_eq!(
        held_reader
            .latest_response_for_request("request-1")
            .and_then(|row| row.content.as_deref()),
        Some(expected_held_content.as_str())
    );
    assert_eq!(
        store
            .snapshot()
            .latest_response_for_request("request-1")
            .and_then(|row| row.content.as_deref()),
        Some("terminal")
    );
}

#[test]
fn observer_never_retains_transcript_content_from_a_mislabeled_patch() {
    let (store, _) = ObservedStore::new(ClientStore::default());
    let initial = store.projection_revision();
    let patch = ClientStore::from_rows(crate::client::store::ClientStoreRows {
        messages: vec![serde_json::from_value(serde_json::json!({
            "message_key": "session-1:1",
            "session_id": "session-1",
            "sequence": 1,
            "role": "user",
            "content": "authoritative row"
        }))
        .expect("message row")],
        ..crate::client::store::ClientStoreRows::default()
    });

    let outcome = store.merge_observer_patch_with_outcome(patch, true);

    assert!(!outcome.response_only);
    assert!(!outcome.copied_snapshot);
    assert!(store.snapshot().messages.is_empty());
    assert_eq!(
        store.projection_revision().reconcile_version,
        initial.reconcile_version + 1
    );
}
use crate::client::schema::ensure_runtime_schemas;
use defra_node::{EventName, NodeBuilder};
use std::sync::Arc;
use tokio::sync::RwLock as AsyncRwLock;

async fn build_observer_fixture() -> (Arc<EmbeddedNode>, Arc<ObservedStore>, ObserverHandle) {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let (store, _rx) = ObservedStore::new(crate::client::store::ClientStore::default());
    let peer_dir = Arc::new(AsyncRwLock::new(
        crate::client::peer_directory::PeerDirectory::load(
            "/tmp/gents-observe-test-peers-nonexistent.json",
        )
        .await
        .expect("peer_directory"),
    ));
    let subscription = node.subscribe(&[EventName::Update]);
    let (_tx, rx) = watch::channel::<Option<String>>(None);
    let handle = spawn_observer_with_selection(
        node.clone(),
        store.clone(),
        peer_dir,
        "did:test:requester".to_string(),
        subscription,
        rx,
    );
    (node, store, handle)
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
                    sequence: {seq},
                    role: "user",
                    content: "{content}",
                    timestamp: "2026-05-07T00:00:00Z"
                }}) {{ _docID }}
            }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

#[tokio::test]
async fn coalesces_burst_into_one_fetch_per_doc() {
    let (node, store, handle) = build_observer_fixture().await;

    let create = r#"mutation {
            create_AgentResponse(input: {
                response_key: "req-1",
                request_id: "req-1",
                agent_did: "did:alpha",
                behavior_id: "default",
                session_id: "sess-1",
                content: "",
                reasoning: "",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 0,
                created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
        }"#;
    let resp = node.execute(create).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);

    let metrics_before = handle.metrics_snapshot();
    for i in 1..=50 {
        let update = format!(
            r#"mutation {{ update_AgentResponse(filter: {{ response_key: {{ _eq: "req-1" }} }}, input: {{ progress_seq: {i} }}) {{ _docID }} }}"#
        );
        let resp = node.execute(&update).await;
        assert!(!resp.has_errors(), "{:?}", resp.errors);
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let metrics_after = handle.metrics_snapshot();

    let fetches = metrics_after.docs_fetched - metrics_before.docs_fetched;
    let flushes = metrics_after.debounce_flushes - metrics_before.debounce_flushes;
    assert!(fetches <= 5, "expected <=5 fetches, got {fetches}");
    assert!(
        flushes >= 1 && flushes <= 5,
        "expected 1..=5 flushes, got {flushes}"
    );

    let snap = store.snapshot();
    let response = snap
        .responses
        .iter()
        .find(|r| r.response_key == "req-1")
        .expect("response present");
    assert_eq!(response.progress_seq, Some(50));

    handle.shutdown().await;
}

#[tokio::test]
async fn multi_collection_burst_fans_out_correctly() {
    let (node, store, handle) = build_observer_fixture().await;

    let create_resp = r#"mutation {
            create_AgentResponse(input: {
                response_key: "req-1",
                request_id: "req-1",
                agent_did: "did:alpha",
                behavior_id: "default",
                session_id: "sess-1",
                content: "",
                reasoning: "",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 0,
                created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
        }"#;
    let resp = node.execute(create_resp).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);

    for i in 1..=5 {
        let update_resp = format!(
            r#"mutation {{ update_AgentResponse(filter: {{ response_key: {{ _eq: "req-1" }} }}, input: {{ progress_seq: {i} }}) {{ _docID }} }}"#
        );
        node.execute(&update_resp).await;

        let create_msg = format!(
            r#"mutation {{
                    create_AgentMessage(input: {{
                        message_key: "sess-1:{i}",
                        session_id: "sess-1",
                        sequence: {i},
                        role: "assistant",
                        content: "msg-{i}",
                        timestamp: "2026-05-07T00:00:0{i}Z"
                    }}) {{ _docID }}
                }}"#
        );
        node.execute(&create_msg).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let snap = store.snapshot();

    assert_eq!(
        snap.responses
            .iter()
            .find(|r| r.response_key == "req-1")
            .and_then(|r| r.progress_seq),
        Some(5),
        "expected progress_seq=5 in responses"
    );

    assert!(
        snap.messages.is_empty(),
        "observer retained transcript rows"
    );
    assert!(handle.metrics_snapshot().transcript_invalidations >= 1);

    handle.shutdown().await;
}

#[tokio::test]
async fn dropped_events_with_no_selection_falls_back_to_full() {
    let (node, store, handle) = build_observer_fixture().await;
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
    let (node, store, handle) = build_observer_fixture().await;

    let before = store.projection_revision();
    seed_message(node.as_ref(), "sess-1", 1, "before-delete").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(store.snapshot().messages.is_empty());
    let after_create = store.projection_revision();
    assert!(after_create.reconcile_version > before.reconcile_version);

    node.execute(
            r#"mutation { delete_AgentMessage(filter: { message_key: { _eq: "sess-1:1" } }) { _docID } }"#,
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(store.snapshot().messages.is_empty());
    assert!(store.projection_revision().reconcile_version > after_create.reconcile_version);
    assert!(handle.metrics_snapshot().transcript_invalidations >= 2);
    handle.shutdown().await;
}

#[tokio::test]
async fn fetch_failures_increment_on_unknown_collection() {
    let (node, _store, handle) = build_observer_fixture().await;

    let result =
        crate::client::query::fetch_doc_patch(node.as_ref(), "NotARealCollection", &["x"]).await;
    assert!(result.is_err(), "expected error for unknown collection");

    let snap = handle.metrics_snapshot();
    assert_eq!(snap.fetch_failures, 0);
    handle.shutdown().await;
}

#[tokio::test]
async fn local_write_increments_redundant_fetch_counter() {
    let (node, _store, handle) = build_observer_fixture().await;

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
