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
        use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use crate::store::identity::NodeIdentity;

        let db = crate::support::test_db("scoped-watcher").await;
        let (home, user, client_access) =
            crate::test_support::signed_test_node_without_agent_from_db(db, "scoped-watcher-user")
                .await;
        let node = home.node.clone();
        let runtime = home.node_identity.clone();
        let binding = crate::store::identity::provision_agent_identity_authorized(
            &node,
            runtime.as_ref(),
            &identity::Did::new(user.did()).unwrap(),
        )
        .await
        .unwrap();
        let profile_id = format!("{}:inference", binding.agent_did);
        let backend_id = format!("{}:backend", binding.agent_did);
        let plan = DesiredStateApplyPlan::new(
            [
                (
                    crate::Collection::InferenceBackend,
                    serde_json::json!({
                        "node_did": runtime.did(),
                        "backend_id": backend_id,
                        "name": "Scoped watcher inference",
                        "provider_kind": "OpenAiCompatible",
                        "endpoint": "http://127.0.0.1:1/v1",
                        "auth": {"kind": "unauthenticated"},
                    }),
                ),
                (
                    crate::Collection::InferenceProfile,
                    serde_json::json!({
                        "node_did": runtime.did(),
                        "profile_id": profile_id,
                        "backend_id": backend_id,
                        "model_name": "test-model",
                    }),
                ),
                (
                    crate::Collection::Agent,
                    serde_json::json!({
                        "node_did": runtime.did(),
                        "agent_did": binding.agent_did,
                        "creation_nonce": binding.creation_nonce,
                        "display_name": "Scoped watcher agent",
                        "inference_profile_id": profile_id,
                        "enabled": true,
                    }),
                ),
            ]
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
        )
        .unwrap();
        client_access
            .transact_write("test.scoped_watcher.agent", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::documents::apply_desired_state_plan(txn, plan).await
                })
            })
            .await
            .unwrap();
        // Production projects the AgentBaseline from each live Agent document
        // (tool permission projection); this fixture has no projection pass, so
        // it grants the same set as the node admin, as the other Agent fixtures do.
        let admin =
            crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
        crate::store::identity::GrantSet::AgentBaseline
            .grant(
                &admin,
                &identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
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

    /// Creates the request as its requesting User through the signed client
    /// path, then grants the selected Agent `working_agent`. An Agent holds
    /// no create right on `delivered_requests`, so it cannot create its own rows.
    async fn create_request(&self, input: serde_json::Value) -> String {
        let agent = identity::Did::new(self.agent.did()).unwrap();
        self.client_access
            .transact_write("test.watcher.request", |txn| {
                let input = &input;
                let agent = &agent;
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                            &serde_json::json!({"input": input}),
                            gents_protocol::schemas::AGENT_REQUEST,
                        )
                        .await?;
                    let doc_id = crate::store::graphql::created_doc_id(&created, "AgentRequest")?;
                    txn.add_actor_relationship("AgentRequest", &doc_id, "working_agent", agent)
                        .await?;
                    Ok::<_, anyhow::Error>(doc_id)
                })
            })
            .await
            .expect("create watcher request for selected agent")
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

    async fn insert_request(
        &self,
        request_id: &str,
        session_id: &str,
        lifecycle_state: &str,
        created_at: &str,
    ) -> String {
        let mut input = self.request_input(request_id, session_id);
        input["lifecycle_state"] = serde_json::json!(lifecycle_state);
        input["created_at"] = serde_json::json!(created_at);
        self.create_request(input).await
    }

    async fn insert_incoherent_request(&self, request_id: &str, session_id: &str) -> String {
        let mut input = self.request_input(request_id, session_id);
        input["request_hop"] = serde_json::json!(1);
        input["caused_by_parent_request_id"] = serde_json::json!("parent-req-exists");
        self.create_request(input).await
    }

    async fn update_request(&self, doc_id: &str, input: serde_json::Value) {
        let doc = crate::store::graphql::escape_graphql_string(doc_id);
        let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
        self.access
            .write(
                "test.watcher.update",
                &format!(
                    r#"mutation {{ update_AgentRequest(docID: "{doc}", input: {input}) {{ _docID }} }}"#
                ),
            )
            .await
            .expect("update watcher request as selected agent");
    }

    async fn set_completed(&self, doc_id: &str) {
        self.update_request(
            doc_id,
            serde_json::json!({ "lifecycle_state": "completed" }),
        )
        .await;
    }

    async fn request_row(&self, doc_id: &str) -> serde_json::Value {
        let doc = crate::store::graphql::escape_graphql_string(doc_id);
        let response = self.access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ lifecycle_state failure_reason claimed_at terminalized_at terminal_redrive_attempts }} }}"#
        )).await.unwrap();
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .unwrap_or_else(|| panic!("selected agent must read request: {response}"));
        let [row] = rows.as_slice() else {
            panic!("selected agent must see one request: {response}");
        };
        row.clone()
    }
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
        let fixture = ScopedWatcherFixture::new().await;
        let session = format!("session-{doc}");
        let head = fixture
            .insert_request("native-head", &session, "pending", "2026-03-12T00:00:00Z")
            .await;
        for request_id in &case.pre.persistent_set {
            fixture
                .insert_request(
                    request_id,
                    &format!("session-{request_id}"),
                    "pending",
                    "2026-03-12T00:00:00Z",
                )
                .await;
        }
        let mut watcher = fixture.watcher();
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
        fixture.set_completed(&head).await;
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
    let fixture = ScopedWatcherFixture::new().await;
    let first = fixture
        .insert_request("z-first", "same-session", "pending", "2026-03-12T00:00:00Z")
        .await;
    let later = fixture
        .insert_request("a-later", "same-session", "pending", "2026-03-12T00:00:00Z")
        .await;
    let watcher = fixture.watcher();
    let held = crate::store::config_client::ConfigApplyTxn::begin_local(&fixture.node, None)
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
    fixture.set_completed(&first).await;
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
    let fixture = ScopedWatcherFixture::new().await;
    let active_doc_id = fixture
        .insert_request(
            "req-active",
            "sess-queue",
            "processing",
            "2026-03-12T00:00:00Z",
        )
        .await;
    fixture
        .insert_request(
            "req-queued",
            "sess-queue",
            "pending",
            "2026-03-12T00:00:01Z",
        )
        .await;
    fixture
        .insert_request("req-other", "sess-other", "pending", "2026-03-12T00:00:02Z")
        .await;

    let watcher = fixture.watcher();
    let pending = watcher.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-other"]
    );

    fixture.set_completed(&active_doc_id).await;
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
    let fixture = ScopedWatcherFixture::new().await;
    fixture
        .insert_request(
            "req-active",
            "sess-queue",
            "processing",
            "2026-03-12T00:00:00Z",
        )
        .await;
    let queued_doc_id = fixture
        .insert_request(
            "req-queued-interrupt",
            "sess-queue",
            "pending",
            "2026-03-12T00:00:01Z",
        )
        .await;
    fixture
        .update_request(
            &queued_doc_id,
            serde_json::json!({ "interrupt_requested_at": chrono::Utc::now().to_rfc3339() }),
        )
        .await;

    let watcher = fixture.watcher();
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
    // The requester owns its request; only its signed deletion removes it.
    fixture
        .client_access
        .transact_write("test.watcher.delete_absent", |txn| {
            let absent = &absent;
            Box::pin(async move { txn.delete_request_for_native_fixture(absent).await })
        })
        .await
        .unwrap();
    let gone = fixture
        .client_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{absent_doc}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    assert!(gone["data"]["AgentRequest"].as_array().unwrap().is_empty());

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
    let fixture = ScopedWatcherFixture::new().await;
    let incoherent_doc_id = fixture
        .insert_incoherent_request("req-incoherent-active", "sess-incoherent")
        .await;
    fixture
        .update_request(
            &incoherent_doc_id,
            serde_json::json!({ "lifecycle_state": "processing" }),
        )
        .await;
    fixture
        .insert_request(
            "req-valid-pending",
            "sess-incoherent",
            "pending",
            "2026-03-12T00:00:01Z",
        )
        .await;

    let watcher = fixture.watcher();
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
    let fixture = ScopedWatcherFixture::new().await;
    let node_did = fixture.node_did.as_str();
    let mut input = fixture.request_input("req-owned", "sess-owned");
    input["created_at"] = serde_json::json!("2026-08-21T00:00:00Z");
    input["workspace_id"] = serde_json::json!("ws-1");
    input["workspace_owner_node_did"] = serde_json::json!(node_did);
    input["workspace_authority"] = serde_json::json!("readWrite");
    fixture.create_request(input).await;

    let foreign = DefraWatcher::for_agents(
        fixture.node.clone(),
        "did:key:foreign",
        vec![fixture.agent.clone()],
    )
    .expect("request watcher");
    assert!(foreign.pending_requests().await.unwrap().is_empty());
    let owner = fixture.watcher();
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
        pending[0].agent_did.clone(),
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
    let agent = identity::Did::new(fixture.agent.did()).unwrap();
    fixture
        .client_access
        .transact_write("test.watcher.session", |txn| {
            let session_input = &session_input;
            let agent = &agent;
            Box::pin(async move {
                let created = txn
                    .execute_client_signed_create(
                        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                        &serde_json::json!({"input": session_input}),
                        gents_protocol::schemas::AGENT_SESSION,
                    )
                    .await?;
                let doc_id = crate::store::graphql::created_doc_id(&created, "AgentSession")?;
                txn.add_actor_relationship("AgentSession", &doc_id, "working_agent", agent)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
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
        continuation,
        60,
        ExecutionOrigin::Interactive,
        "backend",
    );
    lifecycle
        .bind_managed_agent(fixture.agent.as_ref())
        .unwrap();
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
