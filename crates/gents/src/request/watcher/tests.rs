use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use super::cooldown::ProcessedMark;
use super::*;

fn publish_watcher_update(bus: &events::ChannelBus, collection_id: &str, doc_id: &str) {
    use events::Bus;
    let block = format!("{collection_id}:{doc_id}").into_bytes();
    let cid = defra_core::block::generate_cid_from_bytes(&block).unwrap();
    bus.publish(events::Message::update(events::Update::new(
        doc_id.to_owned(),
        cid,
        collection_id.to_owned(),
        block,
        false,
        true,
    )));
}

#[tokio::test(start_paused = true)]
async fn unrelated_changes_do_not_wake_or_postpone_the_request_scan() {
    use events::Bus;
    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    let wait = wait_for_request_change(&mut subscription);
    tokio::pin!(wait);
    for _ in 0..30 {
        publish_watcher_update(&bus, "output", "streaming-output");
        tokio::select! {
            biased;
            result = &mut wait => panic!("unrelated output woke the queue scan: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    assert!(
        wait.await.is_err(),
        "output traffic must not postpone fallback polling"
    );
}

#[tokio::test(start_paused = true)]
async fn request_changes_wake_without_waiting_for_the_fallback() {
    use events::Bus;
    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    publish_watcher_update(&bus, "output", "first");
    publish_watcher_update(&bus, "requests", "second");
    let started = tokio::time::Instant::now();
    let batch = wait_for_request_change(&mut subscription)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokio::time::Instant::now(), started);
    assert_eq!(batch.changes.len(), 1);
    assert_eq!(batch.changes[0].doc_id, "second");
}

#[tokio::test]
async fn a_closed_request_change_feed_stops_the_watcher_wait() {
    use events::Bus;
    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    bus.close();
    assert!(wait_for_request_change(&mut subscription)
        .await
        .unwrap()
        .is_none());
}

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(crate::test_support::node_builder().build().await.unwrap())
}

async fn insert_incoherent_agent_request(
    node: &defra_node::EmbeddedNode,
    node_did: &str,
    request_id: &str,
) -> String {
    use crate::graphql::escape_graphql_string;

    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_node_did = escape_graphql_string(node_did);
    let created_at = chrono::Utc::now().to_rfc3339();

    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                purpose: "normal",
                node_did: "{escaped_node_did}",
                session_id: "sess-incoherent",
                agent_id: "general",
                content: "test",
                lifecycle_state: "pending",
                backend_id: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 0,
                request_hop: 1,
                caused_by_parent_request_id: "parent-req-exists"
            }}) {{ _docID }}
        }}"#
    );

    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create_AgentRequest (incoherent) failed: {:?}",
        response.errors
    );

    let lookup = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&lookup).await;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .expect("AgentRequest _docID")
}

async fn insert_agent_request_row(
    node: &defra_node::EmbeddedNode,
    node_did: &str,
    request_id: &str,
    session_id: &str,
    lifecycle_state: &str,
    created_at: &str,
) -> String {
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let node_did = crate::graphql::escape_graphql_string(node_did);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let lifecycle_state = crate::graphql::escape_graphql_string(lifecycle_state);
    let created_at = crate::graphql::escape_graphql_string(created_at);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                purpose: "normal",
                node_did: "{node_did}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                agent_id: "general",
                content: "test",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create_AgentRequest failed: {:?}",
        response.errors
    );

    let lookup = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&lookup).await;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .expect("AgentRequest _docID")
}

async fn insert_malformed_agent_request_without_request_id(
    node: &defra_node::EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> String {
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{
        purpose: "normal",
        node_did: "{}", session_id: "{}", content: "malformed",
        lifecycle_state: "pending",
        execution_origin: "interactive", created_at: "2026-03-12T00:00:00Z"
    }}) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(node_did),
        crate::graphql::escape_graphql_string(session_id)
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create malformed row: {:?}",
        response.errors
    );
    let lookup = format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(node_did),
        crate::graphql::escape_graphql_string(session_id)
    );
    let response = node.execute(&lookup).await;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|value| value.as_str())
        .expect("malformed row doc id")
        .to_string()
}

async fn request_terminal_fields_by_doc_id(
    node: &defra_node::EmbeddedNode,
    doc_id: &str,
) -> serde_json::Value {
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(
        filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1
    ) {{ lifecycle_state failure_reason claimed_at }} }}"#,
            crate::graphql::escape_graphql_string(doc_id)
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query malformed row: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("malformed terminal row")
}

async fn set_request_terminal_completed(node: &defra_node::EmbeddedNode, doc_id: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{
                    lifecycle_state: "completed"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "update_AgentRequest terminal failed: {:?}",
        response.errors
    );
}

async fn set_request_processing(node: &defra_node::EmbeddedNode, doc_id: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ lifecycle_state: "processing" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "update AgentRequest processing failed: {:?}",
        response.errors
    );
}

async fn set_request_interrupt_requested_at(
    node: &defra_node::EmbeddedNode,
    doc_id: &str,
    at: &str,
) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let at = crate::graphql::escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ interrupt_requested_at: "{at}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "update_AgentRequest interrupt failed: {:?}",
        response.errors
    );
}

async fn request_terminal_fields(
    node: &defra_node::EmbeddedNode,
    request_id: &str,
) -> serde_json::Value {
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 1
            ) {{
                lifecycle_state
                failure_reason
                terminalized_at
                terminal_redrive_attempts
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query AgentRequest terminal fields failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("AgentRequest terminal row")
}

#[tokio::test]
async fn generated_release_cases_preserve_native_session_order() {
    use crate::lean_vocab_test::{lean_event_delivery_transition_cases, LeanEventDeliveryAction};
    let mut cases = 0;
    for case in lean_event_delivery_transition_cases() {
        let LeanEventDeliveryAction::Release { doc } = &case.action else {
            continue;
        };
        cases += 1;
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let owner = "did:key:z-watcher-release";
        let session = format!("session-{doc}");
        let head = insert_agent_request_row(
            node.as_ref(),
            owner,
            "native-head",
            &session,
            "pending",
            "2026-03-12T00:00:00Z",
        )
        .await;
        for request_id in &case.pre.persistent_set {
            insert_agent_request_row(
                node.as_ref(),
                owner,
                request_id,
                &format!("session-{request_id}"),
                "pending",
                "2026-03-12T00:00:00Z",
            )
            .await;
        }
        let mut watcher = DefraWatcher::new(node.clone(), owner).expect("request watcher");
        watcher.processed_request_ids = case
            .pre
            .processed_set
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    ProcessedMark {
                        at: Instant::now(),
                        queue_session: Some(format!("session-{id}")),
                    },
                )
            })
            .collect();
        let selected =
            tokio::time::timeout(std::time::Duration::from_secs(2), watcher.next_request())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert_eq!(selected.request_id, "native-head", "{}", case.name);
        let mut retained = watcher
            .processed_request_ids
            .keys()
            .filter(|id| id.as_str() != "native-head")
            .cloned()
            .collect::<Vec<_>>();
        retained.sort();
        let mut expected = case.post.processed_set.clone();
        expected.sort();
        assert_eq!(retained, expected, "{}", case.name);
        set_request_terminal_completed(node.as_ref(), &head).await;
        let released =
            tokio::time::timeout(std::time::Duration::from_secs(2), watcher.next_request())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        let released_expected = case
            .pre
            .processed_set
            .iter()
            .filter(|id| !case.post.processed_set.contains(id))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            vec![released.request_id],
            released_expected,
            "{}",
            case.name
        );
    }
    assert_eq!(cases, 2);
}

#[tokio::test]
async fn native_arrival_head_is_delivered_despite_reverse_lexical_timestamp_tie() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let owner = "did:key:z-watcher-native-head";
    let first = insert_agent_request_row(
        node.as_ref(),
        owner,
        "z-first",
        "same-session",
        "pending",
        "2026-03-12T00:00:00Z",
    )
    .await;
    let later = insert_agent_request_row(
        node.as_ref(),
        owner,
        "a-later",
        "same-session",
        "pending",
        "2026-03-12T00:00:00Z",
    )
    .await;
    let watcher = DefraWatcher::new(node.clone(), owner).expect("request watcher");
    let held = crate::config_client::ConfigApplyTxn::begin_local(&node, None)
        .await
        .unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(5), watcher.pending_requests())
        .await
        .expect("arrival observation must not wait for the mutation gate")
        .unwrap();
    held.discard().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["z-first"]
    );
    assert!(watcher.try_fetch_request(&later).await.unwrap().is_none());
    assert_eq!(
        watcher
            .try_fetch_request(&first)
            .await
            .unwrap()
            .unwrap()
            .request_id,
        "z-first"
    );
    set_request_terminal_completed(node.as_ref(), &first).await;
    assert_eq!(
        watcher
            .pending_requests()
            .await
            .unwrap()
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-later"]
    );
    assert_eq!(
        watcher
            .try_fetch_request(&later)
            .await
            .unwrap()
            .unwrap()
            .request_id,
        "a-later"
    );
}

#[tokio::test]
async fn pending_requests_skip_queued_same_session_rows_until_claimable() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let node_did = "did:key:z-watcher-queue";
    let active_doc_id = insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-active",
        "sess-queue",
        "processing",
        "2026-03-12T00:00:00Z",
    )
    .await;
    insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-queued",
        "sess-queue",
        "pending",
        "2026-03-12T00:00:01Z",
    )
    .await;
    insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-other",
        "sess-other",
        "pending",
        "2026-03-12T00:00:02Z",
    )
    .await;

    let watcher = DefraWatcher::new(node.clone(), node_did).expect("request watcher");
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-other"]
    );

    set_request_terminal_completed(node.as_ref(), &active_doc_id).await;
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-queued", "req-other"]
    );
}

#[tokio::test]
async fn pending_requests_include_interrupted_queued_rows_for_terminalization() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let node_did = "did:key:z-watcher-queued-interrupt";
    insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-active",
        "sess-queue",
        "processing",
        "2026-03-12T00:00:00Z",
    )
    .await;
    let queued_doc_id = insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-queued-interrupt",
        "sess-queue",
        "pending",
        "2026-03-12T00:00:01Z",
    )
    .await;
    set_request_interrupt_requested_at(
        node.as_ref(),
        &queued_doc_id,
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;

    let watcher = DefraWatcher::new(node, node_did).expect("request watcher");
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-queued-interrupt"]
    );
}

#[tokio::test]
async fn pending_requests_quarantines_incoherent_row_without_hiding_valid_work() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let node_did = "did:key:z-watcher-coherence-pending";
    insert_incoherent_agent_request(node.as_ref(), node_did, "req-incoherent-pending").await;
    insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-valid-pending",
        "sess-incoherent",
        "pending",
        "2026-03-12T00:00:01Z",
    )
    .await;

    let watcher = DefraWatcher::new(node.clone(), node_did).expect("request watcher");
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-valid-pending"],
        "an incoherent row must not poison or queue-block coherent work"
    );
    let terminal = request_terminal_fields(node.as_ref(), "req-incoherent-pending").await;
    assert_eq!(terminal["lifecycle_state"], "failed");
    assert!(terminal["terminalized_at"].is_string());
    assert_eq!(terminal["terminal_redrive_attempts"], 0);
    assert!(terminal["failure_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("incoherent durable lineage")));
}

#[tokio::test]
async fn malformed_pending_row_terminalizes_by_doc_id_without_poisoning_valid_row() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:key:z-watcher-raw-quarantine";
    let malformed_doc = insert_malformed_agent_request_without_request_id(
        node.as_ref(),
        node_did,
        "sess-malformed-a",
    )
    .await;
    insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-valid-b",
        "sess-valid-b",
        "pending",
        "2026-03-12T00:00:01Z",
    )
    .await;

    let watcher = DefraWatcher::new(node.clone(), node_did).expect("request watcher");
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-valid-b"]
    );
    let malformed = request_terminal_fields_by_doc_id(node.as_ref(), &malformed_doc).await;
    assert_eq!(malformed["lifecycle_state"], "failed");
    assert!(malformed["claimed_at"].is_null());
}

#[tokio::test]
async fn incoherent_live_row_still_blocks_a_second_same_session_claim() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let node_did = "did:key:z-watcher-coherence-active";
    let incoherent_doc_id =
        insert_incoherent_agent_request(node.as_ref(), node_did, "req-incoherent-active").await;
    set_request_processing(node.as_ref(), &incoherent_doc_id).await;
    insert_agent_request_row(
        node.as_ref(),
        node_did,
        "req-valid-pending",
        "sess-incoherent",
        "pending",
        "2026-03-12T00:00:01Z",
    )
    .await;

    let watcher = DefraWatcher::new(node, node_did).expect("request watcher");
    assert!(watcher.pending_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn try_fetch_request_terminalizes_incoherent_parent_linkage() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let node_did = "did:key:z-watcher-coherence-fetch";
    let doc_id =
        insert_incoherent_agent_request(node.as_ref(), node_did, "req-incoherent-fetch").await;

    let watcher = DefraWatcher::new(node.clone(), node_did).expect("request watcher");
    let result = watcher.try_fetch_request(&doc_id).await.unwrap();
    assert!(result.is_none());
    let terminal = request_terminal_fields(node.as_ref(), "req-incoherent-fetch").await;
    assert_eq!(terminal["lifecycle_state"], "failed");
    assert!(terminal["terminalized_at"].is_string());
    assert_eq!(terminal["terminal_redrive_attempts"], 0);
}

#[tokio::test]
async fn pending_requests_are_scoped_by_node_did_without_host_identity() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:key:z-watcher-workspace-owner";
    let now = "2026-08-21T00:00:00Z";
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "req-owned",
                purpose: "normal",
                node_did: "{node_did}",
                session_id: "sess-owned",
                retry_parent_request: "",
                retry_root_request: "req-owned",
                superseded_by_request: "",
                agent_id: "general",
                content: "test",
                lifecycle_state: "pending",
                backend_id: "",
                created_at: "{now}",
                retry_count: 0,
                max_retries: 0,
                workspace_id: "ws-1",
                workspace_owner_node_did: "{node_did}",
                workspace_authority: "readWrite"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let foreign = DefraWatcher::new(node.clone(), "did:key:foreign").expect("request watcher");
    assert!(foreign.pending_requests().await.unwrap().is_empty());
    let owner = DefraWatcher::new(node, node_did).expect("request watcher");
    let pending = owner.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-owned"]
    );
    assert_eq!(
        pending[0].workspace_owner_node_did.as_deref(),
        Some(node_did)
    );
    let continuation = crate::request::lifecycle::queue::prepare_goal_continuation(
        &pending[0],
        "general".to_string(),
        "goal-workspace-owner",
        "continue",
        1,
        false,
        "2026-08-21T00:00:01Z",
        pending[0].request_hop,
    )
    .expect("workspace-bound parent produces a coherent goal continuation");
    assert_eq!(
        continuation.workspace_owner_node_did.as_deref(),
        Some(node_did)
    );
}

#[tokio::test]
async fn later_request_cannot_overtake_delivered_native_session_head() {
    use crate::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};

    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:key:z-watcher-requeue";
    let session = "sess-requeue";
    let second = "2026-09-27T19:47:28Z";
    let continuation_id = "goal-cont-00000000000000000003-requeue";
    let wake_id = "background-completion-sess-requeue-00000000000000000000";
    let deliver = Duration::from_secs(5);
    let continuation_doc = insert_agent_request_row(
        node.as_ref(),
        node_did,
        continuation_id,
        session,
        "pending",
        second,
    )
    .await;
    let mut watcher = DefraWatcher::new(node.clone(), node_did).expect("request watcher");
    let continuation = tokio::time::timeout(deliver, watcher.next_request())
        .await
        .expect("first delivery")
        .expect("watcher open")
        .expect("pending scan");
    assert_eq!(continuation.request_id, continuation_id);

    insert_agent_request_row(node.as_ref(), node_did, wake_id, session, "pending", second).await;
    assert!(wake_id < continuation_id);
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        node_did,
        continuation,
        60,
        ExecutionOrigin::Interactive,
        "backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    assert!(watcher.pending_requests().await.unwrap().is_empty());
    set_request_terminal_completed(node.as_ref(), &continuation_doc).await;
    let wake = tokio::time::timeout(deliver, watcher.next_request())
        .await
        .expect("next native arrival")
        .expect("watcher open")
        .expect("pending scan");
    assert_eq!(wake.request_id, wake_id);
}
