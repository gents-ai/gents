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

#[tokio::test(start_paused = true)]
async fn closed_feed_reopens_only_at_fallback_and_wait_is_cancellable() {
    use events::Bus;
    use std::cell::Cell;

    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    bus.close();
    let attempts = Cell::new(0);
    let mut first = Box::pin(wait_for_change_or_resubscribe(&mut subscription, || {
        attempts.set(attempts.get() + 1);
        Err(anyhow::anyhow!("transient subscription failure"))
    }));
    assert!(futures::poll!(first.as_mut()).is_pending());
    tokio::time::advance(GOSSIP_FALLBACK_POLL - Duration::from_millis(1)).await;
    assert!(futures::poll!(first.as_mut()).is_pending());
    assert_eq!(attempts.get(), 0);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(matches!(
        futures::poll!(first.as_mut()),
        std::task::Poll::Ready(Err(_))
    ));
    drop(first);
    assert_eq!(attempts.get(), 1, "observer may re-read after the fallback");

    let mut second = Box::pin(wait_for_change_or_resubscribe(&mut subscription, || {
        attempts.set(attempts.get() + 1);
        Ok(bus.subscribe_collection_changes(vec!["requests".into()]))
    }));
    assert!(futures::poll!(second.as_mut()).is_pending());
    tokio::time::advance(GOSSIP_FALLBACK_POLL - Duration::from_millis(1)).await;
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert_eq!(attempts.get(), 1, "a failed reopen cannot spin");
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(matches!(
        futures::poll!(second.as_mut()),
        std::task::Poll::Ready(Ok(()))
    ));
    drop(second);
    assert_eq!(
        attempts.get(),
        2,
        "observer re-reads after the next fallback"
    );

    let mut cancelled = Box::pin(wait_for_change_or_resubscribe(&mut subscription, || {
        attempts.set(attempts.get() + 1);
        Ok(bus.subscribe_collection_changes(vec!["requests".into()]))
    }));
    assert!(futures::poll!(cancelled.as_mut()).is_pending());
    drop(cancelled);
    tokio::time::advance(GOSSIP_FALLBACK_POLL).await;
    assert_eq!(attempts.get(), 2, "dropping the wait cancels the reopen");
}

#[tokio::test(start_paused = true)]
async fn live_feed_change_does_not_reopen_the_subscription() {
    use events::Bus;

    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    publish_watcher_update(&bus, "requests", "updated");
    let started = tokio::time::Instant::now();
    wait_for_change_or_resubscribe(&mut subscription, || {
        panic!("a live change must not reopen the feed")
    })
    .await
    .unwrap();
    assert_eq!(tokio::time::Instant::now(), started);
}

async fn test_node() -> Arc<defra_node::Node> {
    Arc::new(crate::test_support::node_builder().build().await.unwrap())
}

struct ScopedWatcherFixture {
    node: Arc<defra_node::Node>,
    node_did: String,
    agent: Arc<dyn crate::store::identity::NodeIdentity>,
    access: crate::ConfigAccess,
    client_access: crate::ConfigAccess,
    _home: crate::support::TestDb,
}

impl ScopedWatcherFixture {
    async fn new() -> Self {
        use crate::store::identity::NodeIdentity;

        let db = crate::support::test_signed_db("scoped-watcher").await;
        let (home, _user, client_access, binding) =
            crate::test_support::signed_test_node_from_db(db, "scoped-watcher-user").await;
        let node = home.node.clone();
        let runtime = home.node_identity.clone();
        let agent =
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let access = crate::ConfigAccess::local_managed_agent(
            node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        Self {
            node,
            node_did: runtime.did().to_owned(),
            agent,
            access,
            client_access,
            _home: home,
        }
    }

    fn watcher(&self) -> DefraWatcher {
        DefraWatcher::for_agents(self.node.clone(), &self.node_did, vec![self.agent.clone()])
            .expect("selected-agent request watcher")
    }

    async fn create_request(&self, input: serde_json::Value) -> String {
        let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
        let created = self
            .access
            .write(
                "test.watcher.request",
                &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
            )
            .await
            .expect("create watcher request as selected agent");
        crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap()
    }

    fn request_input(&self, request_id: &str, session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "request_id": request_id,
            "purpose": "normal",
            "node_did": self.node_did,
            "agent_did": self.agent.did(),
            "session_id": session_id,
            "content": "test",
            "lifecycle_state": "pending",
            "execution_origin": "interactive",
            "created_at": "2026-03-12T00:00:01Z",
            "retry_count": 0,
            "max_retries": 0,
        })
    }

    async fn request_row(&self, doc_id: &str) -> serde_json::Value {
        let doc = crate::store::graphql::escape_graphql_string(doc_id);
        let response = self.access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ lifecycle_state failure_reason claimed_at terminalized_at terminal_redrive_attempts }} }}"#
        )).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        let [row] = rows.as_slice() else {
            panic!("selected agent must see one request: {response}");
        };
        row.clone()
    }
}

async fn insert_incoherent_agent_request(
    node: &defra_node::Node,
    node_did: &str,
    request_id: &str,
) -> String {
    use crate::store::graphql::escape_graphql_string;

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
                agent_did: "general",
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
    node: &defra_node::Node,
    node_did: &str,
    request_id: &str,
    session_id: &str,
    lifecycle_state: &str,
    created_at: &str,
) -> String {
    let request_id = crate::store::graphql::escape_graphql_string(request_id);
    let node_did = crate::store::graphql::escape_graphql_string(node_did);
    let session_id = crate::store::graphql::escape_graphql_string(session_id);
    let lifecycle_state = crate::store::graphql::escape_graphql_string(lifecycle_state);
    let created_at = crate::store::graphql::escape_graphql_string(created_at);
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
                agent_did: "general",
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

async fn set_request_terminal_completed(node: &defra_node::Node, doc_id: &str) {
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
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

async fn set_request_processing(node: &defra_node::Node, doc_id: &str) {
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
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

async fn set_request_interrupt_requested_at(node: &defra_node::Node, doc_id: &str, at: &str) {
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
    let at = crate::store::graphql::escape_graphql_string(at);
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
        let mut watcher = DefraWatcher::new_legacy(node.clone(), owner).expect("request watcher");
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
    let watcher = DefraWatcher::new_legacy(node.clone(), owner).expect("request watcher");
    let held = crate::store::config_client::ConfigApplyTxn::begin_local(&node, None)
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

    let watcher = DefraWatcher::new_legacy(node.clone(), node_did).expect("request watcher");
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

    let watcher = DefraWatcher::new_legacy(node, node_did).expect("request watcher");
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
    let fixture = ScopedWatcherFixture::new().await;
    let mut incoherent = fixture.request_input("req-incoherent-pending", "sess-incoherent");
    incoherent["request_hop"] = serde_json::json!(1);
    incoherent["caused_by_parent_request_id"] = serde_json::json!("parent-req-exists");
    let doc_id = fixture.create_request(incoherent).await;
    fixture
        .create_request(fixture.request_input("req-valid-pending", "sess-incoherent"))
        .await;

    let watcher = fixture.watcher();
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-valid-pending"],
        "an incoherent row must not poison or queue-block coherent work"
    );
    let terminal = fixture.request_row(&doc_id).await;
    assert_eq!(terminal["lifecycle_state"], "failed");
    assert!(terminal["terminalized_at"].is_string());
    assert_eq!(terminal["terminal_redrive_attempts"], 0);
    assert!(terminal["failure_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("incoherent durable lineage")));
}

#[tokio::test]
async fn malformed_pending_row_terminalizes_by_doc_id_without_poisoning_valid_row() {
    let fixture = ScopedWatcherFixture::new().await;
    let mut malformed = fixture.request_input("unused", "sess-malformed-a");
    malformed.as_object_mut().unwrap().remove("request_id");
    let malformed_doc = fixture.create_request(malformed).await;
    fixture
        .create_request(fixture.request_input("req-valid-b", "sess-valid-b"))
        .await;

    let watcher = fixture.watcher();
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-valid-b"]
    );
    let malformed = fixture.request_row(&malformed_doc).await;
    assert_eq!(malformed["lifecycle_state"], "failed");
    assert!(malformed["claimed_at"].is_null());
}

#[tokio::test]
async fn malformed_row_without_selected_agent_stays_pending_without_blocking_valid_work() {
    let fixture = ScopedWatcherFixture::new().await;
    let mut malformed = fixture.request_input("unused", "sess-no-agent");
    let fields = malformed.as_object_mut().unwrap();
    fields.remove("request_id");
    fields.remove("agent_did");
    let doc = fixture.create_request(malformed).await;
    fixture
        .create_request(fixture.request_input("req-valid", "sess-valid"))
        .await;

    let pending = fixture.watcher().pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        ["req-valid"]
    );
    let malformed = fixture.request_row(&doc).await;
    assert_eq!(malformed["lifecycle_state"], "pending");
    assert!(malformed["claimed_at"].is_null());
}

#[tokio::test]
async fn unresolved_agent_request_stays_pending_until_identity_is_available() {
    let model = crate::lean_vocab_test::lean_contract_snapshot()
        .watcher_scoped_scan_cases
        .iter()
        .find(|case| case["name"] == "unresolved-agent-is-not-scanned")
        .expect("modeled unresolved agent");
    let fixture = ScopedWatcherFixture::new().await;
    let doc = fixture
        .create_request(fixture.request_input("req-unresolved", "sess-unresolved"))
        .await;
    let unresolved = DefraWatcher::for_agents(fixture.node.clone(), &fixture.node_did, vec![])
        .expect("watcher without a resolved identity");
    let pending = unresolved.pending_requests().await.unwrap();
    assert_eq!(
        pending.len(),
        model["expected"]["rows"].as_array().unwrap().len()
    );
    assert_eq!(
        fixture.request_row(&doc).await["lifecycle_state"],
        "pending"
    );
    let resolved = fixture.watcher().pending_requests().await.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].doc_id, doc);
}

#[tokio::test]
async fn revoked_request_is_not_rejected_or_reported_as_terminalized() {
    use crate::request::admission::{
        terminalize_pending_request_rejection, PendingRejectionOutcome,
    };
    use tracing::instrument::WithSubscriber;

    #[derive(Clone, Default)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    let fixture = ScopedWatcherFixture::new().await;
    let admin =
        crate::ConfigAccess::local_as(fixture.node.clone(), fixture.node_did.parse().unwrap());
    let client: identity::Did = fixture.client_access.actor_did().unwrap().parse().unwrap();
    for permission in [
        crate::store::config_client::NodePermission::DacRelationDelete,
        crate::store::config_client::NodePermission::DocumentDelete,
    ] {
        admin
            .grant_node_permission(&client, permission)
            .await
            .unwrap();
    }
    let mut input = fixture.request_input("req-revoked", "sess-revoked");
    input["request_hop"] = serde_json::json!(1);
    input["caused_by_parent_request_id"] = serde_json::json!("parent-req-exists");
    let created = fixture
        .client_access
        .transact_write("test.watcher.client_request", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &serde_json::json!({"input": input}),
                    gents_protocol::schemas::AGENT_REQUEST,
                )
                .await
            })
        })
        .await
        .unwrap();
    let revoked = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    let actor: identity::Did = fixture.agent.did().parse().unwrap();
    fixture
        .client_access
        .add_actor_relationship("AgentRequest", &revoked, "working_agent", &actor)
        .await
        .unwrap();
    let visible = fixture.request_row(&revoked).await;
    assert_eq!(visible["lifecycle_state"], "pending");
    fixture
        .client_access
        .delete_actor_relationship("AgentRequest", &revoked, "working_agent", &actor)
        .await
        .unwrap();
    let doc = crate::store::graphql::escape_graphql_string(&revoked);
    let hidden = fixture
        .access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    assert!(hidden["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());

    admin
        .grant_node_permission(
            &actor,
            crate::store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    let absent = fixture
        .create_request(fixture.request_input("req-absent", "sess-absent"))
        .await;
    let absent_doc = crate::store::graphql::escape_graphql_string(&absent);
    let deleted = fixture
        .access
        .write(
            "test.watcher.delete_absent",
            &format!(r#"mutation {{ delete_AgentRequest(docID: "{absent_doc}") {{ _docID }} }}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        crate::store::graphql::mutation_documents(&deleted, "delete_AgentRequest")
            .unwrap()
            .unwrap()
            .len(),
        1
    );

    for doc_id in [&revoked, &absent] {
        let result = terminalize_pending_request_rejection(
            &fixture.node,
            doc_id,
            &fixture.node_did,
            fixture.agent.did(),
            fixture.agent.as_ref(),
            "incoherent request",
            "test.watcher.revoked_rejection",
        )
        .await
        .expect("hidden and absent requests do not cause an authorization fallback");
        assert_eq!(result, PendingRejectionOutcome::NotMatched);
    }

    let mut accepted = fixture.request_input("req-visible-incoherent", "sess-visible");
    accepted["request_hop"] = serde_json::json!(1);
    accepted["caused_by_parent_request_id"] = serde_json::json!("parent-req-exists");
    let accepted_doc = fixture.create_request(accepted).await;
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(capture.clone())
        .finish();
    fixture
        .watcher()
        .pending_requests()
        .with_subscriber(subscriber)
        .await
        .unwrap();
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("terminalized incoherent AgentRequest at watcher ingest"),
        "{logs}"
    );
    assert!(logs.contains("req-visible-incoherent"), "{logs}");
    assert!(!logs.contains("req-revoked"), "{logs}");
    assert_eq!(
        fixture.request_row(&accepted_doc).await["lifecycle_state"],
        "failed"
    );
    let row = fixture
        .client_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}) {{ lifecycle_state }} }}"#
        ))
        .await
        .unwrap();
    assert_eq!(row["data"]["AgentRequest"][0]["lifecycle_state"], "pending");
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

    let watcher = DefraWatcher::new_legacy(node, node_did).expect("request watcher");
    assert!(watcher.pending_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn try_fetch_request_terminalizes_incoherent_parent_linkage() {
    let fixture = ScopedWatcherFixture::new().await;
    let mut incoherent = fixture.request_input("req-incoherent-fetch", "sess-incoherent");
    incoherent["request_hop"] = serde_json::json!(1);
    incoherent["caused_by_parent_request_id"] = serde_json::json!("parent-req-exists");
    let doc_id = fixture.create_request(incoherent).await;

    let watcher = fixture.watcher();
    let result = watcher.try_fetch_request(&doc_id).await.unwrap();
    assert!(result.is_none());
    let terminal = fixture.request_row(&doc_id).await;
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
                agent_did: "general",
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

    let foreign =
        DefraWatcher::new_legacy(node.clone(), "did:key:foreign").expect("request watcher");
    assert!(foreign.pending_requests().await.unwrap().is_empty());
    let owner = DefraWatcher::new_legacy(node, node_did).expect("request watcher");
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

    let fixture = ScopedWatcherFixture::new().await;
    let session = "sess-requeue";
    let second = "2026-09-27T19:47:28Z";
    let continuation_id = "goal-cont-00000000000000000003-requeue";
    let wake_id = "background-completion-sess-requeue-00000000000000000000";
    let deliver = Duration::from_secs(5);
    let session_input = serde_json::json!({
        "node_did": fixture.node_did,
        "agent_did": fixture.agent.did(),
        "session_id": session,
        "created_at": second,
    });
    let session_input = gents_protocol::graphql::graphql_input_literal(&session_input).unwrap();
    fixture
        .access
        .write(
            "test.watcher.session",
            &format!("mutation {{ create_AgentSession(input: {session_input}) {{ _docID }} }}"),
        )
        .await
        .expect("create agent-owned session");
    let mut continuation_input = fixture.request_input(continuation_id, session);
    continuation_input["created_at"] = serde_json::json!(second);
    let continuation_doc = fixture.create_request(continuation_input).await;
    let mut watcher = fixture.watcher();
    let continuation = tokio::time::timeout(deliver, watcher.next_request())
        .await
        .expect("first delivery")
        .expect("watcher open")
        .expect("pending scan");
    assert_eq!(continuation.request_id, continuation_id);

    let mut wake_input = fixture.request_input(wake_id, session);
    wake_input["created_at"] = serde_json::json!(second);
    fixture.create_request(wake_input).await;
    assert!(wake_id < continuation_id);
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        fixture.node.clone(),
        fixture.agent.did(),
        &fixture.node_did,
        continuation,
        60,
        ExecutionOrigin::Interactive,
        "backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    assert!(watcher.pending_requests().await.unwrap().is_empty());
    let doc = crate::store::graphql::escape_graphql_string(&continuation_doc);
    let updated = fixture.access.write("test.watcher.complete", &format!(
        r#"mutation {{ update_AgentRequest(docID: "{doc}", input: {{ lifecycle_state: "completed" }}) {{ _docID }} }}"#
    )).await.expect("complete request as selected agent");
    assert_eq!(
        crate::store::graphql::mutation_documents(&updated, "update_AgentRequest")
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    let wake = tokio::time::timeout(deliver, watcher.next_request())
        .await
        .expect("next native arrival")
        .expect("watcher open")
        .expect("pending scan");
    assert_eq!(wake.request_id, wake_id);
}
