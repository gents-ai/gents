use super::*;

#[tokio::test]
async fn event_source_reads_seed_and_group_as_selected_agent() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    node.add_collection("type SelectionProbe { kind: String @index message: String }")
        .await
        .unwrap();
    let response = crate::ConfigAccess::local(node.clone())
        .write("test.event_selection_probe", r#"mutation { create_SelectionProbe(input: {kind: "ready", message: "hello"}) { _docID } }"#)
        .await;
    let id = crate::store::graphql::created_doc_id(&response.unwrap(), "SelectionProbe").unwrap();
    let agent_did = "did:key:z6MkhfKM8cN5hVhu5B4msjSKmz2zPPaTF7s8q5VVt57Uoz6f";
    let task = ResolvedTask {
        agent_did: agent_did.to_owned(),
        ..resolved_task("handle")
    };
    let mut trigger = resolved_event_trigger_with_filter(
        "selection-probe",
        "SelectionProbe",
        task,
        r#"{kind: {_eq: "ready"}}"#,
    );
    let snapshot = snapshot_with_event_triggers(1, RapidHashMap::default());
    let (_tx, rx) = watch::channel(snapshot);
    let source = EventSource::new(rx, node.clone(), CancellationToken::new());

    assert!(event_delivery::probe_document(
        &node,
        &trigger.source_collection,
        &id,
        trigger.filter.as_deref(),
        Some(identity::Did::new(agent_did).unwrap()),
    )
    .await
    .unwrap()
    .is_some());
    assert_eq!(
        source
            .read_source_doc("SelectionProbe", &id, agent_did)
            .await
            .unwrap()
            .unwrap()["message"],
        "hello"
    );

    trigger.fire_mode = crate::runtime::snapshot::EventTriggerFireMode::PerGroup;
    trigger.correlation_field = Some("kind".to_owned());
    trigger.expected_count = Some(1);
    let delivery = event_delivery::Delivery::Trigger {
        node_did: agent_did,
        trigger: &trigger,
    };
    assert!(matches!(
        event_delivery::evaluate_group(
            &node,
            &Default::default(),
            &crate::ConfigAccess::local_as(node.clone(), identity::Did::new(agent_did).unwrap()),
            delivery,
            "missing",
        )
        .await
        .unwrap(),
        event_delivery::GroupOutcome::Empty
    ));
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_seeding_covers_every_document_beyond_the_former_cap() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let ids =
        crate::triggers::engine::event_delivery::source_scan::tests::create_source_documents(&node)
            .await;
    let mut trigger =
        resolved_event_trigger("complete-seed", "RescanSource", resolved_task("observe"));
    trigger.correlation_field = Some("correlation".to_owned());
    trigger.filter = Some("{correlation: {_like: \"group-%\"}}".to_owned());
    let snapshot = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([("complete-seed".to_owned(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.reconcile_subscriptions(&snapshot).await;
    let seen = ids
        .iter()
        .filter(|id| source.has_seen("RescanSource", id))
        .count();
    assert_eq!(
        seen,
        ids.len(),
        "startup seeding must cover the full source collection"
    );
    node.shutdown().await.expect("node shutdown");
}

use events::{
    Bus, ChannelBus, DocumentChangeSubscription, EventName, Message, Subscription, Update,
};

#[derive(Clone)]
struct FakeUpdateSubscriptionSource(Arc<ChannelBus>);

impl FakeUpdateSubscriptionSource {
    fn new() -> Self {
        Self(Arc::new(ChannelBus::new()))
    }

    fn publish_update(&self, collection_id: impl Into<String>, doc_id: impl Into<String>) {
        let collection_id = collection_id.into();
        let doc_id = doc_id.into();
        let block = format!("{collection_id}:{doc_id}").into_bytes();
        let cid = defra_core::block::generate_cid_from_bytes(&block)
            .expect("synthetic update block bytes must produce a CID");
        self.0.publish(Message::update(Update::new(
            doc_id,
            cid,
            collection_id,
            block,
            false,
            true,
        )));
    }
}

impl crate::UpdateSubscriptionSource for FakeUpdateSubscriptionSource {
    fn subscribe_updates(&self) -> Subscription {
        self.0.subscribe(&[EventName::Update])
    }

    fn subscribe_collection_changes(
        &self,
        collection_ids: Vec<String>,
    ) -> DocumentChangeSubscription {
        self.0.subscribe_collection_changes(collection_ids)
    }
}

#[tokio::test]
async fn a_collection_level_update_with_an_empty_doc_id_fires_nothing_but_a_real_one_does() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection("type BranchableProbe { value: String }")
        .await
        .unwrap();

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-branchable".to_string(),
        ..resolved_task("go")
    };
    let trigger = resolved_event_trigger("trigger-branchable", "BranchableProbe", task);
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("trigger-branchable".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let subs = Arc::new(FakeUpdateSubscriptionSource::new());
    let mut source = EventSource::with_subscription_source(
        subs.clone(),
        rx,
        node.clone(),
        CancellationToken::new(),
    );
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let collection_id = node
        .get_collection("BranchableProbe")
        .unwrap()
        .expect("schema registered")
        .collection_id;

    subs.publish_update(collection_id.clone(), "");
    let skipped = tokio::time::timeout(Duration::from_millis(500), source.next_fire()).await;
    assert!(
        skipped.is_err(),
        "a collection-level update with an empty doc id must not fire: {:?}",
        skipped.ok().flatten().map(|intent| intent.event_vars)
    );
    assert!(!source.has_seen("BranchableProbe", ""));

    let response = node
        .execute(r#"mutation { add_BranchableProbe(input: {value: "x"}) { _docID } }"#)
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let doc_id = response.data.unwrap()["add_BranchableProbe"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    subs.publish_update(collection_id, doc_id);
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for the real document update")
        .expect("next_fire returned None for a real document update");
    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-branchable"));
}

#[tokio::test]
async fn an_event_source_on_a_branchable_runtime_collection_delivers_the_document() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let trigger = resolved_event_trigger(
        "receipt-trigger",
        "WorkspaceReceipt",
        resolved_task("{{ event.source_doc_id }}"),
    );
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("receipt-trigger".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    // Receipts are written by the Agent whose workspace produced them.
    let response = crate::triggers::callback::test_agent_access(&node)
        .write(
            "test.workspace_receipt",
            r#"mutation { add_WorkspaceReceipt(input: {
                receipt_id: "receipt-1", workspace_id: "ws-1", kind: "writer"
            }) { _docID } }"#,
        )
        .await
        .unwrap();
    let doc_id = response["data"]["add_WorkspaceReceipt"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();

    let intent = tokio::time::timeout(Duration::from_secs(5), source.next_fire())
        .await
        .expect("a WorkspaceReceipt write must reach the event source")
        .expect("the receipt must deliver");
    assert_eq!(intent.trigger_id.as_deref(), Some("receipt-trigger"));
    assert_eq!(intent.event_vars["source_doc_id"], doc_id);
    assert!(!source.has_seen("WorkspaceReceipt", ""));
}

type ReconciliationRecord = (u64, String, Result<(), String>);

#[derive(Default)]
struct SubscriptionObserver(std::sync::Mutex<Vec<ReconciliationRecord>>);

impl crate::RuntimeSnapshotObserver for SubscriptionObserver {
    fn on_generation_published(&self, _generation: u64, _fingerprint: &str, _behaviors: &[String]) {
    }

    fn on_event_sources_reconciled(
        &self,
        generation: u64,
        fingerprint: &str,
        result: Result<(), &str>,
    ) {
        self.0.lock().unwrap().push((
            generation,
            fingerprint.to_owned(),
            result.map_err(str::to_owned),
        ));
    }
}

#[tokio::test]
async fn event_source_acknowledges_exact_configuration_after_subscription_setup() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection("type ReadyInput { value: String }")
        .await
        .unwrap();
    assert!(!node
        .execute(r#"mutation { create_ReadyInput(input: {value: "before"}) {_docID} }"#)
        .await
        .has_errors());
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([(
            "ready-trigger".into(),
            resolved_event_trigger("ready-trigger", "ReadyInput", resolved_task("observe")),
        )]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let observer = Arc::new(SubscriptionObserver::default());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new())
        .with_runtime_observer(Some(observer.clone()));
    assert!(observer.0.lock().unwrap().is_empty());
    source.reconcile_subscriptions(&snapshot).await;
    assert_eq!(
        *observer.0.lock().unwrap(),
        vec![(1, snapshot.configuration_fingerprint(), Ok(()))]
    );
    let submitted = node
        .execute(r#"mutation { create_ReadyInput(input: {value: "after"}) {_docID} }"#)
        .await;
    assert!(!submitted.has_errors());
    let intent = tokio::time::timeout(Duration::from_secs(5), source.next_fire())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        intent.doc_vars.as_ref().unwrap()["value"],
        "after",
        "pre-ack documents must have been seeded before readiness"
    );
}

#[tokio::test]
async fn event_source_retries_schema_visibility_before_subscription_readiness() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let snapshot = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([(
            "delayed-schema".into(),
            resolved_event_trigger(
                "delayed-schema",
                "DelayedSchemaInput",
                resolved_task("observe"),
            ),
        )]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let observer = Arc::new(SubscriptionObserver::default());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new())
        .with_runtime_observer(Some(observer.clone()));
    let schema_node = node.clone();
    let install = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        schema_node
            .add_collection("type DelayedSchemaInput { value: String }")
            .await
            .unwrap();
    });

    source.reconcile_subscriptions(&snapshot).await;
    install.await.unwrap();

    assert_eq!(
        *observer.0.lock().unwrap(),
        vec![(1, snapshot.configuration_fingerprint(), Ok(()))]
    );
}

#[tokio::test]
async fn event_source_does_not_acknowledge_failed_seeding_on_later_generations() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let triggers = RapidHashMap::from_iter([(
        "missing".into(),
        resolved_event_trigger("missing", "AbsentInput", resolved_task("observe")),
    )]);
    let first = snapshot_with_event_triggers(1, triggers.clone());
    let (_tx, rx) = watch::channel(first.clone());
    let observer = Arc::new(SubscriptionObserver::default());
    let mut source = EventSource::new(rx, node, CancellationToken::new())
        .with_runtime_observer(Some(observer.clone()));
    source.reconcile_subscriptions(&first).await;
    source
        .reconcile_subscriptions(&snapshot_with_event_triggers(2, triggers))
        .await;
    let failures = observer.0.lock().unwrap().clone();
    assert_eq!(failures.len(), 2);
    assert!(failures
        .iter()
        .all(|(_, _, result)| result.as_ref().unwrap_err().contains("AbsentInput")));
    let removed = snapshot_with_event_triggers(3, RapidHashMap::new());
    source.reconcile_subscriptions(&removed).await;
    assert_eq!(
        observer.0.lock().unwrap().last(),
        Some(&(3, removed.configuration_fingerprint(), Ok(())))
    );
}

#[tokio::test]
async fn event_source_repairs_failed_seed_on_later_reconcile_before_readiness() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let triggers = RapidHashMap::from_iter([(
        "eventual".into(),
        resolved_event_trigger("eventual", "EventualInput", resolved_task("observe")),
    )]);
    let first = snapshot_with_event_triggers(1, triggers.clone());
    let (_tx, rx) = watch::channel(first.clone());
    let observer = Arc::new(SubscriptionObserver::default());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new())
        .with_runtime_observer(Some(observer.clone()));

    source.reconcile_subscriptions(&first).await;
    node.add_collection("type EventualInput { value: String }")
        .await
        .unwrap();
    let second = snapshot_with_event_triggers(2, triggers);
    source.reconcile_subscriptions(&second).await;

    let observations = observer.0.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert!(observations[0].2.is_err());
    assert_eq!(
        observations[1],
        (2, second.configuration_fingerprint(), Ok(()))
    );
}

fn resolved_event_trigger(
    trigger_id: &str,
    source_collection: &str,
    task: ResolvedTask,
) -> ResolvedEventTrigger {
    ResolvedEventTrigger {
        event_source_id: trigger_id.to_owned(),
        event_source_doc_id: "source-doc".into(),
        task_doc_id: "task-doc".into(),
        session_id_template: None,
        trigger_doc_id: format!("{trigger_id}-doc"),
        trigger_id: trigger_id.to_string(),
        task_id: task.task_id.clone(),
        task,
        source_collection: source_collection.to_string(),
        event_kind: "created".to_string(),
        filter: None,
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
        fire_mode: crate::runtime::snapshot::EventTriggerFireMode::PerDocument,
        correlation_field: None,
        expected_count: None,
        expected_count_field: None,
        group_timeout_secs: None,
        group_min_count: 1,
        workspace_authority: None,
    }
}

fn resolved_event_trigger_with_filter(
    trigger_id: &str,
    source_collection: &str,
    task: ResolvedTask,
    filter: &str,
) -> ResolvedEventTrigger {
    ResolvedEventTrigger {
        event_source_id: trigger_id.to_owned(),
        event_source_doc_id: "source-doc".into(),
        task_doc_id: "task-doc".into(),
        session_id_template: None,
        trigger_doc_id: format!("{trigger_id}-doc"),
        trigger_id: trigger_id.to_string(),
        task_id: task.task_id.clone(),
        task,
        source_collection: source_collection.to_string(),
        event_kind: "created".to_string(),
        filter: Some(filter.to_string()),
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
        fire_mode: crate::runtime::snapshot::EventTriggerFireMode::PerDocument,
        correlation_field: None,
        expected_count: None,
        expected_count_field: None,
        group_timeout_secs: None,
        group_min_count: 1,
        workspace_authority: None,
    }
}

async fn hydrate_event_config_ids(
    node: &Arc<defra_node::Node>,
    trigger: &mut ResolvedEventTrigger,
) {
    let response = crate::triggers::callback::test_agent_access(node).execute(&format!(
        "{{Trigger(filter:{{trigger_id:{{_eq:\"{}\"}}}}){{_docID}} EventSource(filter:{{event_source_id:{{_eq:\"{}\"}}}}){{_docID}} Task(filter:{{task_id:{{_eq:\"{}\"}}}}){{_docID}}}}",
        escape_graphql_string(&trigger.trigger_id), escape_graphql_string(&trigger.event_source_id),
        escape_graphql_string(&trigger.task_id))).await.unwrap();
    trigger.trigger_doc_id = response["data"]["Trigger"][0]["_docID"]
        .as_str()
        .unwrap()
        .into();
    trigger.event_source_doc_id = response["data"]["EventSource"][0]["_docID"]
        .as_str()
        .unwrap()
        .into();
    trigger.task_doc_id = response["data"]["Task"][0]["_docID"]
        .as_str()
        .unwrap()
        .into();
}

fn event_test_behavior() -> Arc<ResolvedAgent> {
    static BEHAVIOR: std::sync::OnceLock<Arc<ResolvedAgent>> = std::sync::OnceLock::new();
    BEHAVIOR
        .get_or_init(|| integration_test_agent("general"))
        .clone()
}

/// Event-source tasks run as the event behavior's signed Agent; its snapshot
/// only holds that Agent, so the task must name its DID.
fn resolved_task(prompt_template: &str) -> ResolvedTask {
    ResolvedTask {
        agent_did: event_test_behavior().agent_did.clone(),
        ..super::resolved_task(prompt_template)
    }
}

fn snapshot_with_event_triggers(
    generation: u64,
    triggers: RapidHashMap<String, ResolvedEventTrigger>,
) -> Arc<ActiveRuntimeSnapshot> {
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        event_test_behavior().agent_did.clone(),
        vec![event_test_behavior()],
        RapidHashMap::new(),
        RapidHashMap::new(),
        RapidHashMap::new(),
    )
    .with_automation(crate::runtime::snapshot::ResolvedAutomation {
        event_triggers: triggers,
        ..Default::default()
    })
    .with_node(stub_principal());
    Arc::new(resolved.activate(generation, RapidHashMap::new()))
}

/// The fixture Node's managed Agent; durable arrival delivery reads and seeds
/// cursors through this Agent's signer, which must belong to the test node.
fn configured_event_agent() -> Arc<ResolvedAgent> {
    static AGENT: std::sync::OnceLock<Arc<ResolvedAgent>> = std::sync::OnceLock::new();
    AGENT
        .get_or_init(|| {
            use crate::store::identity::NodeIdentity;
            let root = crate::triggers::callback::test_root_identity();
            let mut agent = event_test_behavior().as_ref().clone();
            agent.agent_identity = crate::triggers::callback::test_agent_identity();
            agent.agent_did = agent.agent_identity.did().to_owned();
            agent.node = Arc::new(RuntimeNode {
                node_did: root.did().to_owned(),
                identity: Arc::new(root),
                default_agent_did: agent.agent_did.clone(),
                display_name: None,
                enabled: true,
            });
            Arc::new(agent)
        })
        .clone()
}

/// A node whose identity, Node row and Agent match `configured_event_agent`.
async fn configured_event_node() -> Arc<defra_node::Node> {
    let node = Arc::new(
        crate::triggers::callback::test_node_builder()
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();
    crate::triggers::callback::seed_test_agent(
        &node,
        crate::request::lifecycle::test_support::PIN_FIXED_DID,
    )
    .await;
    node
}

/// Persist each trigger's Task, EventSource and Trigger as the owning user so
/// the configuration transaction seeds its arrival cursor, then bind the
/// resolved triggers to those documents and the fixture Agent.
async fn configured_event_snapshot(
    node: &Arc<defra_node::Node>,
    generation: u64,
    triggers: RapidHashMap<String, ResolvedEventTrigger>,
) -> Arc<ActiveRuntimeSnapshot> {
    let agent = configured_event_agent();
    let mut configured = RapidHashMap::with_capacity(triggers.len());
    for (id, mut trigger) in triggers {
        trigger.task.agent_did = agent.agent_did.clone();
        let group = (trigger.fire_mode == crate::runtime::snapshot::EventTriggerFireMode::PerGroup)
            .then(|| {
                let mut group = serde_json::json!({});
                if let Some(count) = trigger.expected_count {
                    group["expected_count"] = count.into();
                }
                if let Some(field) = &trigger.expected_count_field {
                    group["expected_count"] = serde_json::json!({"source_field": field});
                }
                if let Some(timeout) = trigger.group_timeout_secs {
                    group["timeout_secs"] = timeout.into();
                }
                if trigger.group_min_count != 1 {
                    group["min_count"] = trigger.group_min_count.into();
                }
                group
            });
        crate::triggers::callback::seed_test_configuration(
            node,
            crate::request::lifecycle::test_support::PIN_FIXED_DID,
            vec![
                (
                    crate::Collection::Task,
                    serde_json::json!({"task_id": trigger.task_id,
                        "agent_did": trigger.task.agent_did,
                        "prompt_template": trigger.task.prompt_template,
                        "emit_outcome": trigger.task.emit_outcome}),
                ),
                (
                    crate::Collection::EventSource,
                    serde_json::json!({"event_source_id": trigger.event_source_id,
                        "source_collection": trigger.source_collection,
                        "event_kind": trigger.event_kind,
                        "filter": trigger.filter,
                        "correlation_field": trigger.correlation_field,
                        "group": group}),
                ),
                (
                    crate::Collection::Trigger,
                    serde_json::json!({"trigger_id": trigger.trigger_id,
                        "task_id": trigger.task_id,
                        "enabled": trigger.enabled,
                        "concurrency": trigger.concurrency,
                        "source": {"kind": "event", "event_source_id": trigger.event_source_id}}),
                ),
            ],
        )
        .await;
        hydrate_event_config_ids(node, &mut trigger).await;
        configured.insert(id, trigger);
    }
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        agent.agent_did.clone(),
        vec![agent.clone()],
        RapidHashMap::new(),
        RapidHashMap::new(),
        RapidHashMap::new(),
    )
    .with_node(agent.node.clone())
    .with_local_did(crate::request::lifecycle::test_support::PIN_FIXED_DID.into())
    .with_automation(crate::runtime::snapshot::ResolvedAutomation {
        event_triggers: configured,
        ..Default::default()
    });
    Arc::new(resolved.activate(generation, RapidHashMap::new()))
}

async fn admit_observed_event(node: &Arc<defra_node::Node>, intent: &FireIntent) -> FireResult {
    admit_observed_event_for(node, intent, event_test_behavior().node_did()).await
}

async fn admit_observed_event_for(
    node: &Arc<defra_node::Node>,
    intent: &FireIntent,
    owner_did: &str,
) -> FireResult {
    use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
    let identity = FireIdentity {
        owner_did: owner_did.to_owned(),
        trigger_id: intent.trigger_id.clone().unwrap(),
        source_collection: intent.event_vars["source_collection"]
            .as_str()
            .unwrap()
            .into(),
        source_doc_id: intent.event_vars["source_doc_id"].as_str().unwrap().into(),
    };
    let fire = TriggerFire {
        fire_key: identity.fire_key(),
        request_id: identity.request_id(),
        session_id: identity.session_id(),
        identity,
        task_id: intent.task.task_id.clone(),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: false,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2030-01-01T00:00:00Z".into(),
    };
    let mutation = format!("mutation {{create_AgentRequest(input: {{request_id: \"{}\", node_did: \"{}\", session_id: \"{}\", agent_did: \"{}\", content: \"fixture delivery\", purpose: \"normal\", lifecycle_state: \"pending\", created_at: \"2030-01-01T00:00:00Z\"}}) {{_docID}}}}",
        escape_graphql_string(&fire.request_id), escape_graphql_string(&fire.identity.owner_did), escape_graphql_string(&fire.session_id), escape_graphql_string(&intent.task.agent_did));
    crate::store::config_client::ConfigAccess::local(node.clone())
        .transact_write("test.event_admission", |txn| {
            let fire = &fire;
            let mutation = &mutation;
            Box::pin(async move {
                super::super::durable::stage_fire_request(txn, fire, mutation)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();
    FireResult::Fired {
        request_id: fire.request_id,
    }
}

async fn admit_selected_event(
    node: &Arc<defra_node::Node>,
    intent: &FireIntent,
    owner: &str,
) -> FireResult {
    let agent = crate::triggers::callback::test_agent_identity();
    let access = crate::triggers::callback::test_agent_access(node);
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: owner.into(),
        trigger_id: intent.trigger_id.clone().unwrap(),
        source_collection: intent.event_vars["source_collection"]
            .as_str()
            .unwrap()
            .into(),
        source_doc_id: intent.event_vars["source_doc_id"].as_str().unwrap().into(),
    };
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: identity.fire_key(),
        request_id: identity.request_id(),
        session_id: identity.session_id(),
        identity,
        task_id: intent.task.task_id.clone(),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: intent.task.emit_outcome,
        queued_serial: intent.concurrency == ConcurrencyMode::QueuedSerial,
        source_handoff_id: intent
            .doc_vars
            .as_ref()
            .and_then(|doc| doc["handoff_id"].as_str())
            .map(str::to_owned),
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2030-01-01T00:00:00Z".into(),
    };
    let response = access
        .execute(&format!(
            "{{Trigger(filter:{{trigger_id:{{_eq:\"{}\"}}}}){{_docID}}}}",
            escape_graphql_string(intent.trigger_id.as_deref().unwrap())
        ))
        .await
        .unwrap();
    let trigger_doc = response["data"]["Trigger"][0]["_docID"].as_str().unwrap();
    let spec = crate::request::lifecycle::materialize::pending_agent_request_spec(
        owner,
        agent.did(),
        &intent.task.prompt_template,
        crate::request::lifecycle::ExecutionOrigin::Scheduled,
        crate::request::lifecycle::TriggerLineage {
            trigger_id: intent.trigger_id.clone(),
            trigger_kind: Some("event".into()),
            source_doc_id: Some(fire.identity.source_doc_id.clone()),
            correlation: intent.correlation.clone(),
            trigger_context: None,
        },
        None,
        None,
        &fire.request_id,
        &fire.session_id,
        Some(&fire.fire_key),
        None,
        Some(trigger_doc),
    )
    .unwrap();
    let create = crate::request::lifecycle::build_signed_request(
        spec,
        crate::request::lifecycle::RequestSigner::Identity(agent.as_ref()),
    )
    .await
    .unwrap();
    let prepared = super::super::durable::PreparedFire {
        receipt: fire.clone(),
        target_existing: false,
        replace_prior: false,
        event_source_generation: intent.event_source_generation.clone(),
    };
    let result = crate::request::lifecycle::materialize::write_trigger_delivery_with_identity(
        node,
        identity::Did::new(agent.did()).unwrap(),
        &prepared,
        &create,
        Some(agent.as_ref()),
    )
    .await
    .unwrap();
    FireResult::Fired {
        request_id: result.request.request_id,
    }
}

#[tokio::test]
async fn event_source_reconciles_subscriptions_on_generation_bump() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let task = resolved_task("ignored");
    let snap1 = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([(
            "trigger-a".to_string(),
            resolved_event_trigger("trigger-a", "CollectionA", task.clone()),
        )]),
    );
    let (snapshot_tx, snapshot_rx) = watch::channel(snap1.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(snapshot_rx, node.clone(), cancel.clone());

    source.reconcile_subscriptions(snap1.as_ref()).await;

    assert_eq!(
        source.subscribed_collections(),
        vec!["CollectionA".to_string()],
    );

    let snap2 = snapshot_with_event_triggers(
        2,
        RapidHashMap::from_iter([(
            "trigger-b".to_string(),
            resolved_event_trigger("trigger-b", "CollectionB", task),
        )]),
    );
    snapshot_tx.send(snap2.clone()).expect("snapshot_rx alive");

    source.reconcile_subscriptions(snap2.as_ref()).await;

    assert_eq!(
        source.subscribed_collections(),
        vec!["CollectionB".to_string()],
    );
}

#[tokio::test]
async fn event_source_next_fire_emits_intent_on_matching_real_event() {
    let node = configured_event_node().await;
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");
    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-webhook".to_string(),
        ..resolved_task("handle webhook")
    };
    let trigger = resolved_event_trigger("trigger-webhook", "WebhookEvent", task.clone());
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("trigger-webhook".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    assert_eq!(
        source.subscribed_collections(),
        vec!["WebhookEvent".to_string()],
        "precondition: subscription set should match the trigger's source_collection",
    );
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-1",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors
        );
    });
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for WebhookEvent")
        .expect("next_fire returned None instead of emitting a FireIntent");
    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-webhook"));
    assert_eq!(intent.trigger_kind, TriggerKind::Event);
    assert_eq!(intent.concurrency, ConcurrencyMode::Serial);
    assert_eq!(intent.task.task_id, "task-webhook");
    assert_eq!(intent.task.prompt_template, "handle webhook");
    let doc_vars = intent
        .doc_vars
        .as_ref()
        .expect("every fire must hydrate doc_vars when the filter is None");
    assert_eq!(
        doc_vars["external_id"].as_str(),
        Some("wh-1"),
        "doc_vars must project the WebhookEvent fields, got {doc_vars}"
    );
    assert!(intent.args_vars.is_none());
    let ev = &intent.event_vars;
    assert_eq!(ev["trigger_id"].as_str(), Some("trigger-webhook"));
    assert_eq!(ev["trigger_kind"].as_str(), Some("event"));
    assert_eq!(ev["source_collection"].as_str(), Some("WebhookEvent"));
    assert!(
        ev["source_doc_id"].as_str().is_some_and(|s| !s.is_empty()),
        "source_doc_id should be a non-empty string from the persisted doc, got {:?}",
        ev["source_doc_id"]
    );
    assert!(
        ev["fired_at"].is_string(),
        "fired_at should be a string, got {:?}",
        ev["fired_at"]
    );
    assert_eq!(
        intent.durable_fire_key,
        crate::triggers::engine::durable_fire_key(
            "event-document",
            &["trigger-webhook", ev["source_doc_id"].as_str().unwrap()]
        )
    );
}

#[tokio::test]
async fn a_callback_result_fires_its_bindings_event_source_once() {
    use crate::store::identity::NodeIdentity as _;
    let root = crate::triggers::callback::test_root_identity();
    let owner = root.did().to_owned();
    let node = Arc::new(
        crate::triggers::callback::test_node_builder()
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::triggers::callback::seed_test_agent(&node, &owner).await;

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-after-callback".to_string(),
        agent_did: crate::triggers::callback::test_agent_did(),
        ..resolved_task("continue after the callback")
    };
    let mut trigger = resolved_event_trigger_with_filter(
        "trigger-after-callback",
        "CallbackResult",
        task,
        r#"{ binding_id: { _eq: "maintenance-execute-workspace" } }"#,
    );
    crate::triggers::callback::seed_test_configuration(&node, &owner, vec![
        (crate::Collection::Task, serde_json::json!({"task_id":"task-after-callback",
            "agent_did":crate::triggers::callback::test_agent_did(), "prompt_template":"continue after the callback"})),
        (crate::Collection::EventSource, serde_json::json!({"event_source_id":"trigger-after-callback",
            "source_collection":"CallbackResult", "event_kind":"created",
            "filter":"{ binding_id: { _eq: \"maintenance-execute-workspace\" } }"})),
        (crate::Collection::Trigger, serde_json::json!({"trigger_id":"trigger-after-callback",
            "task_id":"task-after-callback", "concurrency":"serial",
            "source":{"kind":"event", "event_source_id":"trigger-after-callback"}})),
    ]).await;
    hydrate_event_config_ids(&node, &mut trigger).await;
    let mut behavior = event_test_behavior().as_ref().clone();
    behavior.agent_identity = crate::triggers::callback::test_agent_identity();
    behavior.agent_did = behavior.agent_identity.did().to_owned();
    behavior.node = Arc::new(crate::store::identity::RuntimeNode {
        node_did: owner.clone(),
        identity: Arc::new(root),
        default_agent_did: behavior.agent_did.clone(),
        display_name: None,
        enabled: true,
    });
    let principal = behavior.node.clone();
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        behavior.agent_did.clone(),
        vec![Arc::new(behavior)],
        RapidHashMap::new(),
        RapidHashMap::new(),
        RapidHashMap::new(),
    )
    .with_node(principal)
    .with_local_did(owner.clone())
    .with_automation(crate::runtime::snapshot::ResolvedAutomation {
        event_triggers: RapidHashMap::from_iter([("trigger-after-callback".to_string(), trigger)]),
        ..Default::default()
    });
    let snapshot = Arc::new(resolved.activate(1, RapidHashMap::new()));
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_write = node.clone();
    let owner_for_write = owner.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        for (invocation_id, binding_id) in [
            ("inv-other", "another-binding"),
            ("inv-wanted", "maintenance-execute-workspace"),
        ] {
            crate::triggers::callback::create_callback_result(
                &node_for_write,
                &crate::triggers::callback::test_agent_access(&node_for_write),
                &crate::triggers::callback::CallbackResultDoc {
                    result_id: format!("res-{invocation_id}"),
                    invocation_id: invocation_id.to_string(),
                    binding_id: Some(binding_id.to_string()),
                    agent_did: crate::triggers::callback::test_agent_did(),
                    owner_node_did: owner_for_write.clone(),
                    workspace_id: None,
                    work_unit_id: None,
                    caused_by_correlation: None,
                    created_at: None,
                },
            )
            .await
            .expect("CallbackResult written");
        }
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for CallbackResult")
        .expect("next_fire returned None");
    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-after-callback"));
    let doc_vars = intent.doc_vars.as_ref().expect("hydrated result");
    assert_eq!(doc_vars["invocation_id"].as_str(), Some("inv-wanted"));

    let result = admit_selected_event(&node, &intent, &owner).await;
    (intent.on_result)(result);
    let extra = tokio::time::timeout(Duration::from_millis(500), source.next_fire()).await;
    assert!(
        extra.is_err(),
        "fired more than once: {:?}",
        extra.ok().flatten().map(|intent| intent.event_vars)
    );
}

#[tokio::test]
async fn per_group_startup_recovery_uses_filtered_membership_and_deterministic_scope() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection(
        r#"type GroupMember {
            run_id: String
            expected_total: Int
            kind: String
            value: String
        }"#,
    )
    .await
    .expect("group source schema");

    for (run_id, expected, kind, value) in [
        ("run-a", 2, "include", "second"),
        ("run-a", 2, "exclude", "must-not-count"),
        ("run-b", 2, "include", "partial"),
        ("run-a", 2, "include", "first"),
    ] {
        let mutation = format!(
            r#"mutation {{ create_GroupMember(input: {{
                run_id: "{run_id}", expected_total: {expected}, kind: "{kind}", value: "{value}"
            }}) {{ _docID }} }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
    }

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "group-task".to_string(),
        ..resolved_task("{{ group.correlation_value }} {{ group.count }}")
    };
    let trigger = ResolvedEventTrigger {
        event_source_id: "source".into(),
        event_source_doc_id: "source-doc".into(),
        task_doc_id: "task-doc".into(),
        fire_mode: crate::runtime::snapshot::EventTriggerFireMode::PerGroup,
        correlation_field: Some("run_id".into()),
        expected_count_field: Some("expected_total".into()),
        filter: Some(r#"{ kind: { _eq: "include" } }"#.into()),
        ..resolved_event_trigger("group-trigger", "GroupMember", task)
    };
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("group-trigger".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node, CancellationToken::new());

    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("startup recovery timeout")
        .expect("complete group must be recovered");
    assert_eq!(intent.correlation.as_deref(), Some("run-a"));
    assert_eq!(intent.event_vars["correlation"], "run-a");
    let group = intent.group_vars.expect("group scope");
    assert_eq!(group["count"], 2);
    assert_eq!(group["complete"], true);
    let docs = group["docs"].as_array().expect("group docs");
    assert_eq!(docs.len(), 2);
    assert!(docs.iter().all(|doc| doc["kind"] == "include"));
    let doc_ids = docs
        .iter()
        .map(|doc| doc["_docID"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(doc_ids.windows(2).all(|pair| pair[0] < pair[1]));
}

#[tokio::test]
async fn per_group_timeout_uses_durable_first_seen_clock() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection(
        r#"type DurableGroupMember {
            run_id: String
            value: String
        }"#,
    )
    .await
    .expect("group source schema");
    let response = node
        .execute(
            r#"mutation {
                create_DurableGroupMember(input: { run_id: "run-old", value: "partial" }) {
                    _docID
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);

    let task = resolved_task("{{ group.correlation_value }} {{ group.complete }}");
    let trigger = ResolvedEventTrigger {
        fire_mode: crate::runtime::snapshot::EventTriggerFireMode::PerGroup,
        correlation_field: Some("run_id".into()),
        expected_count: Some(2),
        group_timeout_secs: Some(60),
        group_min_count: 1,
        ..resolved_event_trigger("durable-group-trigger", "DurableGroupMember", task)
    };
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("durable-group-trigger".to_string(), trigger)]),
    )
    .await;
    // The owning Agent created this group clock before the runtime restarted;
    // its first observation is already past the group timeout.
    let trigger = snapshot.active_event_triggers()["durable-group-trigger"].clone();
    let owner = configured_event_agent().node_did().to_owned();
    let (group_key, consumer_config_key) =
        EventSource::group_state_keys(&owner, &trigger, "run-old");
    let state = gents_protocol::event_delivery::EventGroupState {
        group_key,
        node_did: owner.clone(),
        consumer: gents_protocol::event_delivery::EventConsumer::Trigger {
            trigger_id: trigger.trigger_id.clone(),
        },
        correlation: "run-old".into(),
        consumer_config_key,
        first_seen_at: (Utc::now() - ChronoDuration::seconds(120))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        quiesced_at: None,
        quiesced_reason: None,
    };
    let agent_did = configured_event_agent().agent_did.clone();
    crate::triggers::callback::test_agent_access(&node)
        .transact_write("test.durable_group_clock", |txn| {
            let (owner, state, agent_did) = (&owner, &state, &agent_did);
            Box::pin(async move {
                let (parent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    owner,
                    agent_did,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("fixture Agent is unavailable"))?;
                let created = txn
                    .execute_with_variables(
                        "mutation($input:EventGroupStateMutationInputArg!){create_EventGroupState(input:$input){_docID}}",
                        &serde_json::json!({"input": state}),
                    )
                    .await?;
                let doc = crate::store::graphql::created_doc_id(&created, "EventGroupState")?;
                txn.add_resource_relationship("EventGroupState", &doc, "parent", "agents", &parent)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node, CancellationToken::new());

    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("durably elapsed timeout recovery")
        .expect("partial group above the floor must fire");
    assert_eq!(intent.correlation.as_deref(), Some("run-old"));
    assert_eq!(intent.group_vars.as_ref().unwrap()["complete"], false);
}

#[tokio::test]
async fn correlation_populated_after_create_remains_eligible_for_delivery() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection(
        r#"type DeferredCorrelationMember {
            run_id: String @index
            value: String
        }"#,
    )
    .await
    .expect("deferred correlation source schema");

    let response = node
        .execute(
            r#"mutation {
                create_DeferredCorrelationMember(input: { value: "created-first" }) {
                    _docID
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let lookup = node
        .execute(r#"query { DeferredCorrelationMember { _docID } }"#)
        .await;
    assert!(!lookup.has_errors(), "{:#?}", lookup.errors);
    let doc_id = lookup
        .data
        .as_ref()
        .and_then(|data| data.get("DeferredCorrelationMember"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("created document id")
        .to_string();

    let trigger = ResolvedEventTrigger {
        correlation_field: Some("run_id".into()),
        ..resolved_event_trigger(
            "deferred-correlation-trigger",
            "DeferredCorrelationMember",
            resolved_task("{{ event.correlation }}"),
        )
    };
    let snapshot = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([("deferred-correlation-trigger".to_string(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.deferrals =
        crate::triggers::engine::deferred_delivery::DeferralWatch::new(Duration::ZERO);

    source.reconcile_subscriptions(snapshot.as_ref()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(500), source.next_fire())
            .await
            .is_err(),
        "a row without its correlation value must not fire"
    );
    assert_eq!(source.deferrals.stuck(), 1);
    let mutation = format!(
        r#"mutation {{
            update_DeferredCorrelationMember(
                docID: "{}",
                input: {{ run_id: "run-late" }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(&doc_id),
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:#?}", response.errors);

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("follow-up correlation update timed out")
        .expect("follow-up correlation update must deliver the created document");
    assert_eq!(intent.correlation.as_deref(), Some("run-late"));
    assert_eq!(intent.event_vars["source_doc_id"], doc_id);
    assert_eq!(source.deferrals.pending(), 0);
}

#[tokio::test]
async fn generated_sibling_delivery_case_preserves_pending_correlation() {
    use crate::lean_vocab_test::{lean_event_delivery_transition_cases, LeanEventDeliveryAction};

    let cases = lean_event_delivery_transition_cases();
    let case = cases
        .iter()
        .find(|case| case.name == "handle_ready_trigger_preserves_pending_sibling")
        .expect("Lean sibling delivery case");
    let LeanEventDeliveryAction::Handle { doc: ready_key } = &case.action else {
        panic!("sibling case must observe a handled delivery");
    };
    let pending_key = case
        .pre
        .subscription_queue
        .iter()
        .find(|key| *key != ready_key)
        .expect("pending sibling input");
    let (ready_id, logical_doc) = ready_key.split_once(':').expect("ready delivery identity");
    let (pending_id, pending_doc) = pending_key
        .split_once(':')
        .expect("pending delivery identity");
    assert_eq!(
        logical_doc, pending_doc,
        "siblings must share one physical source row"
    );
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection(
        r#"type MixedCorrelationMember {
            run_id: String @index
            value: String
        }"#,
    )
    .await
    .expect("mixed correlation source schema");

    let ready = resolved_event_trigger(ready_id, "MixedCorrelationMember", resolved_task("ready"));
    let pending = ResolvedEventTrigger {
        correlation_field: Some("run_id".into()),
        ..resolved_event_trigger(
            pending_id,
            "MixedCorrelationMember",
            resolved_task("{{ event.correlation }}"),
        )
    };
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([
            (ready.trigger_id.clone(), ready),
            (pending.trigger_id.clone(), pending),
        ]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let response = node
        .execute(
            r#"mutation {
                create_MixedCorrelationMember(input: { value: "created-first" }) {
                    _docID
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let lookup = node
        .execute(r#"query { MixedCorrelationMember { _docID } }"#)
        .await;
    assert!(!lookup.has_errors(), "{:#?}", lookup.errors);
    let doc_id = lookup.data.as_ref().unwrap()["MixedCorrelationMember"][0]["_docID"]
        .as_str()
        .expect("created document id")
        .to_string();

    let first = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("ready sibling delivery timed out")
        .expect("ready sibling must not be blocked by missing correlation");
    assert_eq!(first.event_vars["source_doc_id"], doc_id);
    let observed_ready = format!(
        "{}:{logical_doc}",
        first.trigger_id.as_deref().expect("ready trigger")
    );
    assert_eq!(
        vec![observed_ready],
        case.post.handled,
        "observed ready delivery"
    );

    let result = admit_observed_event(&node, &first).await;
    (first.on_result)(result);

    let mutation = format!(
        r#"mutation {{
            update_MixedCorrelationMember(
                docID: "{}",
                input: {{ run_id: "run-late" }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(&doc_id),
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:#?}", response.errors);

    let second = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("deferred sibling delivery timed out")
        .expect("deferred sibling must become eligible when correlation arrives");
    assert_eq!(second.event_vars["source_doc_id"], doc_id);
    let observed_pending = format!(
        "{}:{logical_doc}",
        second.trigger_id.as_deref().expect("pending trigger")
    );
    assert_eq!(
        vec![observed_pending],
        case.post.subscription_queue,
        "pending sibling remains deliverable after its correlation arrives"
    );
    assert_eq!(second.correlation.as_deref(), Some("run-late"));
}

#[tokio::test]
async fn startup_seed_defers_only_the_incomplete_trigger_on_a_shared_document() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection(
        r#"type SeededMixedCorrelationMember {
            run_id: String @index
            value: String
        }"#,
    )
    .await
    .expect("seeded mixed correlation source schema");
    let response = node
        .execute(
            r#"mutation {
                create_SeededMixedCorrelationMember(input: { value: "pre-existing" }) {
                    _docID
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let lookup = node
        .execute(r#"query { SeededMixedCorrelationMember { _docID } }"#)
        .await;
    assert!(!lookup.has_errors(), "{:#?}", lookup.errors);
    let doc_id = lookup.data.as_ref().unwrap()["SeededMixedCorrelationMember"][0]["_docID"]
        .as_str()
        .expect("created document id")
        .to_string();

    let ready = resolved_event_trigger(
        "trigger-a-ready",
        "SeededMixedCorrelationMember",
        resolved_task("ready"),
    );
    let pending = ResolvedEventTrigger {
        correlation_field: Some("run_id".into()),
        ..resolved_event_trigger(
            "trigger-z-pending",
            "SeededMixedCorrelationMember",
            resolved_task("{{ event.correlation }}"),
        )
    };
    let snapshot = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([
            (ready.trigger_id.clone(), ready),
            (pending.trigger_id.clone(), pending),
        ]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let mutation = format!(
        r#"mutation {{
            update_SeededMixedCorrelationMember(
                docID: "{}",
                input: {{ run_id: "run-seeded-late" }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(&doc_id),
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:#?}", response.errors);

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("deferred seeded delivery timed out")
        .expect("only the correlation-incomplete trigger should remain eligible");
    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-z-pending"));
    assert_eq!(intent.correlation.as_deref(), Some("run-seeded-late"));
}

#[tokio::test]
async fn invalid_group_is_durably_quiesced_and_pruned_after_restart() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection(
        r#"type QuiescedGroupMember {
            run_id: String @index
            value: String
        }"#,
    )
    .await
    .expect("quiesced group source schema");
    for value in ["one", "two"] {
        let mutation = format!(
            r#"mutation {{
                create_QuiescedGroupMember(input: {{ run_id: "run-overfull", value: "{value}" }}) {{
                    _docID
                }}
            }}"#,
        );
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
    }

    let trigger = ResolvedEventTrigger {
        fire_mode: crate::runtime::snapshot::EventTriggerFireMode::PerGroup,
        correlation_field: Some("run_id".into()),
        expected_count: Some(1),
        ..resolved_event_trigger(
            "quiesced-group-trigger",
            "QuiescedGroupMember",
            resolved_task("{{ group.count }}"),
        )
    };
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("quiesced-group-trigger".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut first_source = EventSource::new(rx, node.clone(), CancellationToken::new());
    first_source
        .reconcile_subscriptions(snapshot.as_ref())
        .await;
    assert_eq!(first_source.group_membership_query_count(), 1);

    let response = node
        .execute(
            r#"query {
                EventGroupState(
                    limit: 2
                ) { quiesced_at quiesced_reason }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("EventGroupState"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .expect("durable group state");
    assert!(row.get("quiesced_at").is_some_and(|value| !value.is_null()));
    assert!(row["quiesced_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("exceeds expected count")));

    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut restarted_source = EventSource::new(rx, node, CancellationToken::new());
    restarted_source
        .reconcile_subscriptions(snapshot.as_ref())
        .await;
    assert_eq!(restarted_source.group_membership_query_count(), 0);
}

#[test]
fn group_state_identity_changes_only_with_membership_definition() {
    let task = resolved_task("first prompt");
    let mut trigger = ResolvedEventTrigger {
        fire_mode: crate::runtime::snapshot::EventTriggerFireMode::PerGroup,
        correlation_field: Some("run_id".into()),
        expected_count: Some(2),
        ..resolved_event_trigger("group-trigger", "GroupMember", task)
    };
    let initial =
        EventSource::group_state_keys(configured_event_agent().node_did(), &trigger, "run-a");

    trigger.task.prompt_template = "changed prompt".into();
    trigger.expected_count = Some(3);
    assert_eq!(
        EventSource::group_state_keys(configured_event_agent().node_did(), &trigger, "run-a"),
        initial,
        "task and policy changes must not restart a group's first-seen clock",
    );

    trigger.filter = Some(r#"{ kind: { _eq: "include" } }"#.into());
    assert_ne!(
        EventSource::group_state_keys(configured_event_agent().node_did(), &trigger, "run-a"),
        initial,
        "membership filter changes must use fresh recovery state",
    );
}

#[tokio::test]
async fn generation_bump_recovers_only_when_group_membership_definition_changes() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_collection(
        r#"type RecoveryFingerprintMember {
            run_id: String
            kind: String
        }"#,
    )
    .await
    .expect("group source schema");

    let task = resolved_task("first prompt");
    let trigger = ResolvedEventTrigger {
        fire_mode: crate::runtime::snapshot::EventTriggerFireMode::PerGroup,
        correlation_field: Some("run_id".into()),
        expected_count: Some(2),
        ..resolved_event_trigger("fingerprint-trigger", "RecoveryFingerprintMember", task)
    };
    let second_trigger = ResolvedEventTrigger {
        trigger_id: "fingerprint-trigger-2".into(),
        ..trigger.clone()
    };
    let snapshot_one = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([
            ("fingerprint-trigger".to_string(), trigger.clone()),
            ("fingerprint-trigger-2".to_string(), second_trigger.clone()),
        ]),
    );
    let (_tx, rx) = watch::channel(snapshot_one.clone());
    let mut source = EventSource::new(rx, node, CancellationToken::new());
    source.reconcile_subscriptions(snapshot_one.as_ref()).await;
    assert_eq!(source.group_recovery_page_query_count(), 1);

    let mut task_only_change = trigger.clone();
    task_only_change.task.prompt_template = "second prompt".into();
    let snapshot_two = snapshot_with_event_triggers(
        2,
        RapidHashMap::from_iter([
            ("fingerprint-trigger".to_string(), task_only_change),
            ("fingerprint-trigger-2".to_string(), second_trigger.clone()),
        ]),
    );
    source.reconcile_subscriptions(snapshot_two.as_ref()).await;
    assert_eq!(
        source.group_recovery_page_query_count(),
        1,
        "an unrelated snapshot generation bump must not launch recovery I/O",
    );

    let mut membership_change = trigger;
    membership_change.filter = Some(r#"{ kind: { _eq: "include" } }"#.into());
    let snapshot_three = snapshot_with_event_triggers(
        3,
        RapidHashMap::from_iter([
            ("fingerprint-trigger".to_string(), membership_change),
            ("fingerprint-trigger-2".to_string(), second_trigger),
        ]),
    );
    source
        .reconcile_subscriptions(snapshot_three.as_ref())
        .await;
    assert_eq!(
        source.group_recovery_page_query_count(),
        2,
        "a membership-definition change must restart bounded recovery",
    );
}

#[tokio::test]
async fn event_source_filter_probe_gates_fire_on_operator_filter() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // `kind` must be indexed for DefraDB's filter evaluator to accept `_eq`
    // on a non-_docID field in a limit-1 query.
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
            kind: String @index
            email: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-webhook".to_string(),
        ..resolved_task("handle webhook")
    };
    let trigger = resolved_event_trigger_with_filter(
        "trigger-filtered",
        "WebhookEvent",
        task.clone(),
        r#"{ kind: { _eq: "signup" } }"#,
    );
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("trigger-filtered".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let other_mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-other",
                payload: "{}",
                kind: "other",
                email: "other@example.com"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(other_mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent(other) failed: {:?}",
            resp.errors
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let signup_mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-signup",
                payload: "{}",
                kind: "signup",
                email: "alice@example.com"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(signup_mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent(signup) failed: {:?}",
            resp.errors
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out; filter-probe must yield for the signup doc")
        .expect("next_fire returned None instead of emitting a FireIntent");

    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-filtered"));
    assert_eq!(intent.trigger_kind, TriggerKind::Event);
    assert_eq!(
        intent.event_vars["source_collection"].as_str(),
        Some("WebhookEvent"),
    );
    let doc_vars = intent
        .doc_vars
        .as_ref()
        .expect("filter-matched fire must carry hydrated doc_vars");
    assert_eq!(
        doc_vars["kind"].as_str(),
        Some("signup"),
        "hydrated doc_vars must reflect the matching doc, got {doc_vars}"
    );
    assert_eq!(doc_vars["external_id"].as_str(), Some("wh-signup"));

    cancel.cancel();
}

#[tokio::test]
async fn event_source_hydrates_doc_vars_from_source_doc_fields() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
            kind: String @index
            email: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-webhook".to_string(),
        ..resolved_task("handle webhook")
    };
    let trigger = resolved_event_trigger("trigger-hydrate", "WebhookEvent", task.clone());
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("trigger-hydrate".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-hydrate",
                payload: "{\"foo\":1}",
                kind: "signup",
                email: "bob@example.com"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent(hydrate) failed: {:?}",
            resp.errors
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out; hydration path should yield on any created doc")
        .expect("next_fire returned None instead of emitting a FireIntent");

    let doc_vars = intent
        .doc_vars
        .as_ref()
        .expect("FireIntent must carry hydrated doc_vars");
    assert_eq!(
        doc_vars["external_id"].as_str(),
        Some("wh-hydrate"),
        "doc_vars must project `external_id`, got {doc_vars}"
    );
    assert_eq!(
        doc_vars["kind"].as_str(),
        Some("signup"),
        "doc_vars must project `kind`, got {doc_vars}"
    );
    assert_eq!(
        doc_vars["email"].as_str(),
        Some("bob@example.com"),
        "doc_vars must project `email`, got {doc_vars}"
    );
    assert_eq!(
        doc_vars["payload"].as_str(),
        Some(r#"{"foo":1}"#),
        "doc_vars must project `payload`, got {doc_vars}"
    );
    assert!(
        doc_vars["_docID"].as_str().is_some_and(|s| !s.is_empty()),
        "doc_vars must always carry _docID, got {doc_vars}"
    );

    cancel.cancel();
}

async fn create_event_trigger_doc(
    node: &Arc<defra_node::Node>,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
) {
    let input = serde_json::json!({"node_did":event_test_behavior().node_did(),"trigger_id":trigger_id,"task_id":task_id,"source":{"kind":"event","event_source_id":source_collection},"enabled":true,"concurrency":"serial","fire_count":0});
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "test.trigger_fixture",
        |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_with_variables(
                "mutation($input:TriggerMutationInputArg!){create_Trigger(input:$input){_docID}}",
                &serde_json::json!({"input":input}),
            )
            .await?;
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_trigger_fire_count_updates_are_not_lost() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let trigger_id = "trigger-concurrent-fire-count";
    create_event_trigger_doc(&node, trigger_id, "task-webhook", "WebhookEvent").await;
    let node_did = event_test_behavior().node_did().to_owned();

    let updates = (0..12)
        .map(|index| {
            let node = node.clone();
            let node_did = node_did.clone();
            tokio::spawn(async move {
                crate::documents::update_trigger_runtime_fields(
                    &node,
                    &node_did,
                    trigger_id,
                    crate::documents::TriggerRuntimeUpdate {
                        last_attempt_at: Some(format!("2026-01-01T00:00:{index:02}Z")),
                        last_status: Some("fired".to_owned()),
                        fire_count_delta: Some(1),
                        ..Default::default()
                    },
                )
                .await
            })
        })
        .collect::<Vec<_>>();
    let mut committed = 0;
    for update in updates {
        match update.await.unwrap() {
            Ok(()) => committed += 1,
            Err(error) => assert!(
                crate::store::config_client::is_classified_transaction_conflict(&error),
                "only bounded conflict exhaustion may refuse a concurrent update: {error:#}"
            ),
        }
    }
    assert!(committed > 0, "at least one concurrent update commits");
    let trigger =
        observed_trigger(node.as_ref(), event_test_behavior().node_did(), trigger_id).await;
    assert_eq!(trigger["fire_count"].as_i64(), Some(committed));
}

async fn observed_trigger(node: &defra_node::Node, owner_did: &str, id: &str) -> serde_json::Value {
    let query = format!(
        "{{Trigger(filter:{{node_did:{{_eq:\"{}\"}},trigger_id:{{_eq:\"{}\"}}}},limit:2){{task_id source enabled concurrency last_status last_error last_attempt_at last_fired_source_doc_id fire_count}}}}",
        escape_graphql_string(owner_did),
        escape_graphql_string(id)
    );
    let response = node.execute(&query).await;
    let rows = crate::store::graphql::rows::<serde_json::Value>(&response, "Trigger").unwrap();
    assert_eq!(rows.len(), 1, "scoped trigger disappeared or is ambiguous");
    rows[0].clone()
}

#[tokio::test]
async fn event_source_on_result_writes_runtime_fields_on_fired() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-webhook".to_string(),
        ..resolved_task("handle webhook")
    };
    let mut trigger = resolved_event_trigger("trigger-fired", "WebhookEvent", task.clone());
    trigger.event_source_id = "WebhookEvent".into();
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("trigger-fired".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-fire",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for WebhookEvent")
        .expect("next_fire returned None instead of emitting a FireIntent");

    let fired_source_doc_id = intent
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str())
        .expect("event_vars.source_doc_id must be a string")
        .to_string();

    (intent.on_result)(FireResult::Fired {
        request_id: "req-0".to_string(),
    });

    let mut fired_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let trig = observed_trigger(
            node.as_ref(),
            configured_event_agent().node_did(),
            "trigger-fired",
        )
        .await;
        if trig["last_status"].as_str() == Some("fired") {
            fired_trigger = Some(trig);
            break;
        }
    }
    let fired = fired_trigger.expect("Trigger.last_status never became \"fired\"");
    assert_eq!(fired["last_status"].as_str(), Some("fired"));
    assert_eq!(fired["fire_count"].as_i64(), Some(1));
    assert_eq!(
        fired["last_fired_source_doc_id"].as_str(),
        Some(fired_source_doc_id.as_str())
    );
    assert!(fired["last_attempt_at"].as_str().is_some());
    assert_eq!(fired["last_error"], serde_json::Value::Null,);
    assert_eq!(fired["task_id"].as_str(), Some("task-webhook"));
    assert_eq!(
        fired["source"]["event_source_id"].as_str(),
        Some("WebhookEvent")
    );
    assert_eq!(fired["source"]["kind"].as_str(), Some("event"));
    assert_eq!(fired["enabled"].as_bool(), Some(true));
    assert_eq!(fired["concurrency"].as_str(), Some("serial"));

    cancel.cancel();
}

#[tokio::test]
async fn event_source_on_result_writes_runtime_fields_on_skipped_or_errored() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-webhook".to_string(),
        ..resolved_task("handle webhook")
    };
    let mut trigger = resolved_event_trigger("trigger-skip-err", "WebhookEvent", task.clone());
    trigger.event_source_id = "WebhookEvent".into();
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("trigger-skip-err".to_string(), trigger)]),
    )
    .await;
    crate::documents::update_trigger_runtime_fields(
        &node,
        configured_event_agent().node_did(),
        "trigger-skip-err",
        crate::documents::TriggerRuntimeUpdate {
            fire_count_delta: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-skip",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out waiting for WebhookEvent")
        .expect("next_fire returned None instead of emitting a FireIntent");

    let trigger_id = "trigger-skip-err".to_string();
    let source_doc_id = intent
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str())
        .expect("event_vars.source_doc_id must be a string")
        .to_string();

    (intent.on_result)(FireResult::Skipped {
        reason: "serial: prior fire still in-flight".to_string(),
    });

    let mut skipped_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let trig = observed_trigger(
            node.as_ref(),
            configured_event_agent().node_did(),
            "trigger-skip-err",
        )
        .await;
        if trig["last_status"].as_str() == Some("skipped") {
            skipped_trigger = Some(trig);
            break;
        }
    }
    let skipped = skipped_trigger.expect("Trigger.last_status never became \"skipped\"");
    assert_eq!(skipped["last_status"].as_str(), Some("skipped"));
    assert_eq!(skipped["fire_count"].as_i64(), Some(0));
    assert_eq!(
        skipped["last_error"].as_str(),
        Some("serial: prior fire still in-flight"),
    );
    assert!(skipped["last_attempt_at"].as_str().is_some());
    assert_eq!(
        skipped["last_fired_source_doc_id"].as_str(),
        Some(source_doc_id.as_str()),
    );
    assert_eq!(skipped["task_id"].as_str(), Some("task-webhook"));
    assert_eq!(skipped["enabled"].as_bool(), Some(true));
    assert_eq!(skipped["concurrency"].as_str(), Some("serial"));

    EventSource::spawn_runtime_field_write(
        node.clone(),
        configured_event_agent().node_did().to_owned(),
        trigger_id.clone(),
        source_doc_id.clone(),
        FireResult::Errored {
            error: "materializer failed: backend timeout".to_string(),
        },
    );

    let mut errored_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let trig = observed_trigger(
            node.as_ref(),
            configured_event_agent().node_did(),
            "trigger-skip-err",
        )
        .await;
        if trig["last_status"].as_str() == Some("error") {
            errored_trigger = Some(trig);
            break;
        }
    }
    let errored = errored_trigger.expect("Trigger.last_status never became \"error\"");
    assert_eq!(errored["last_status"].as_str(), Some("error"));
    assert_eq!(errored["fire_count"].as_i64(), Some(0));
    assert_eq!(
        errored["last_error"].as_str(),
        Some("materializer failed: backend timeout"),
    );
    assert_eq!(errored["task_id"].as_str(), Some("task-webhook"));
    assert_eq!(errored["enabled"].as_bool(), Some(true));
    assert_eq!(errored["concurrency"].as_str(), Some("serial"));

    cancel.cancel();
}

#[tokio::test]
async fn event_source_skips_event_for_doc_already_seen_at_subscribe() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");

    let seed_mutation = r#"mutation {
        create_WebhookEvent(input: {
            external_id: "wh-preexisting",
            payload: "seed"
        }) { _docID }
    }"#;
    let resp = node.execute(seed_mutation).await;
    assert!(
        !resp.has_errors(),
        "seeding pre-existing doc failed: {:?}",
        resp.errors,
    );
    let lookup = r#"query {
        WebhookEvent(filter: { external_id: { _eq: "wh-preexisting" } }, limit: 1) {
            _docID
        }
    }"#;
    let resp = node.execute(lookup).await;
    assert!(
        !resp.has_errors(),
        "lookup of pre-existing doc failed: {:?}",
        resp.errors,
    );
    let preexisting_doc_id = resp
        .data
        .as_ref()
        .and_then(|d| d.get("WebhookEvent"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .expect("WebhookEvent query returned no _docID")
        .to_string();

    let task = resolved_task("ignored");
    let trigger = resolved_event_trigger("trigger-noupdate", "WebhookEvent", task);
    let snapshot = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([("trigger-noupdate".to_string(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    let escaped = escape_graphql_string(&preexisting_doc_id);
    let update_mutation = format!(
        r#"mutation {{
            update_WebhookEvent(
                docID: "{escaped}",
                input: {{ payload: "updated" }}
            ) {{ _docID }}
        }}"#
    );
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let resp = node_for_mutation.execute(&update_mutation).await;
        assert!(
            !resp.has_errors(),
            "update_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });

    let result = tokio::time::timeout(Duration::from_millis(500), source.next_fire()).await;
    assert!(
        result.is_err(),
        "next_fire yielded an intent for a pre-seeded doc's update",
    );

    cancel.cancel();
}

#[tokio::test]
async fn event_source_fires_for_first_seen_doc_then_skips_updates() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");
    let task = resolved_task("ignored");
    let trigger = resolved_event_trigger("trigger-firstseen", "WebhookEvent", task);
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("trigger-firstseen".to_string(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let node_for_create = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-first",
                payload: "{}"
            }) { _docID }
        }"#;
        let resp = node_for_create.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out on first observation (create should fire)")
        .expect("next_fire returned None instead of emitting a FireIntent");
    assert_eq!(intent.trigger_id.as_deref(), Some("trigger-firstseen"));
    let doc_id = intent
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str())
        .expect("source_doc_id must be a string")
        .to_string();
    let escaped = escape_graphql_string(&doc_id);
    let update_mutation = format!(
        r#"mutation {{
            update_WebhookEvent(
                docID: "{escaped}",
                input: {{ payload: "updated" }}
            ) {{ _docID }}
        }}"#
    );
    let node_for_update = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let resp = node_for_update.execute(&update_mutation).await;
        assert!(
            !resp.has_errors(),
            "update_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });
    let result = tokio::time::timeout(Duration::from_millis(500), source.next_fire()).await;
    assert!(
        result.is_err(),
        "next_fire yielded an intent for a doc's update; first-seen gate failed to \
         suppress the second observation",
    );
    cancel.cancel();
}

#[tokio::test]
async fn event_source_fans_out_one_event_across_multiple_matching_triggers() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");
    let task = resolved_task("ignored");
    let trigger_alpha = resolved_event_trigger("trigger-alpha", "WebhookEvent", task.clone());
    let trigger_beta = resolved_event_trigger("trigger-beta", "WebhookEvent", task);
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([
            ("trigger-alpha".to_string(), trigger_alpha),
            ("trigger-beta".to_string(), trigger_beta),
        ]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-fanout",
                payload: "{}"
            }) { _docID }
        }"#;
    let resp = node.execute(mutation).await;
    assert!(
        !resp.has_errors(),
        "create_WebhookEvent failed: {:?}",
        resp.errors,
    );
    let first = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out on the first fan-out intent")
        .expect("next_fire returned None instead of emitting the first intent");
    let result = admit_observed_event(&node, &first).await;
    (first.on_result)(result);
    let second = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out on the second fan-out intent; fan-out dropped it?")
        .expect("next_fire returned None instead of emitting the second intent");
    assert_eq!(
        first.trigger_id.as_deref(),
        Some("trigger-alpha"),
        "fan-out must emit intents in deterministic lex-by-trigger_id order",
    );
    assert_eq!(second.trigger_id.as_deref(), Some("trigger-beta"));
    let first_doc_id = first
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str());
    let second_doc_id = second
        .event_vars
        .get("source_doc_id")
        .and_then(|v| v.as_str());
    assert_eq ! (first_doc_id , second_doc_id , "both fan-out intents must carry the same source_doc_id: {first_doc_id:?} vs {second_doc_id:?}" ,);
    cancel.cancel();
}

#[tokio::test]
async fn event_source_tries_all_triggers_when_first_filter_misses() {
    let node = configured_event_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let webhook_sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_collection(webhook_sdl)
        .await
        .expect("add_collection for WebhookEvent");
    let task = resolved_task("ignored");
    let trigger_a = resolved_event_trigger_with_filter(
        "trigger-a-lex-first",
        "WebhookEvent",
        task.clone(),
        r#"{ kind: { _eq: "signup" } }"#,
    );
    let trigger_b = resolved_event_trigger_with_filter(
        "trigger-b-matches",
        "WebhookEvent",
        task,
        r#"{ kind: { _eq: "other" } }"#,
    );
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([
            ("trigger-a-lex-first".to_string(), trigger_a),
            ("trigger-b-matches".to_string(), trigger_b),
        ]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let node_for_mutation = node.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mutation = r#"mutation {
            create_WebhookEvent(input: {
                external_id: "wh-missfirst",
                payload: "{}",
                kind: "other"
            }) { _docID }
        }"#;
        let resp = node_for_mutation.execute(mutation).await;
        assert!(
            !resp.has_errors(),
            "create_WebhookEvent failed: {:?}",
            resp.errors,
        );
    });
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect(
            "next_fire timed out; trigger-a's filter miss silently dropped the \
             event for trigger-b (fan-out regression)",
        )
        .expect("next_fire returned None instead of emitting a FireIntent");
    assert_eq!(
        intent.trigger_id.as_deref(),
        Some("trigger-b-matches"),
        "after trigger-a filter-miss, the engine must still try trigger-b and fire \
         for it; got trigger_id = {:?}",
        intent.trigger_id,
    );
    let maybe_extra = tokio::time::timeout(Duration::from_millis(300), source.next_fire()).await;
    assert!(
        maybe_extra.is_err(),
        "trigger-a emitted a FireIntent despite its filter miss",
    );
    cancel.cancel();
}

#[tokio::test]
async fn event_source_reconcile_excludes_invalid_source_collection_identifiers() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let task = resolved_task("ignored");
    let snapshot = snapshot_with_event_triggers(
        1,
        RapidHashMap::from_iter([
            (
                "trigger-injection".to_string(),
                resolved_event_trigger(
                    "trigger-injection",
                    "Msg(limit: 1) { _docID } Foo",
                    task.clone(),
                ),
            ),
            (
                "trigger-introspection".to_string(),
                resolved_event_trigger("trigger-introspection", "__Type", task.clone()),
            ),
            (
                "trigger-clean".to_string(),
                resolved_event_trigger("trigger-clean", "CollectionClean", task),
            ),
        ]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());

    let cancel = CancellationToken::new();
    let mut source = EventSource::new(rx, node.clone(), cancel.clone());
    source.reconcile_subscriptions(snapshot.as_ref()).await;

    assert_eq!(
        source.subscribed_collections(),
        vec!["CollectionClean".to_string()],
    );
}

#[test]
fn source_fields_query_refuses_a_hostile_collection_name() {
    for hostile in [
        r#"Job") { fields { name } } __schema { types { name } } x: __type(name: "Job"#,
        "Job\" } } # ",
        "__Type",
        "Job Other",
    ] {
        crate::triggers::engine::event_source::source_fields_query(hostile)
            .expect_err(&format!("{hostile:?} must be refused"));
    }
    let query =
        crate::triggers::engine::event_source::source_fields_query("Job").expect("a plain name");
    assert!(query.contains(r#"__type(name: "Job")"#), "{query}");
}

#[path = "event_source/resumed_goal.rs"]
mod resumed_goal;

#[path = "event_source/unacknowledged.rs"]
mod unacknowledged;
