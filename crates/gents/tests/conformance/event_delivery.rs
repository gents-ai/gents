use super::*;
use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::{EmbeddedNode, QueryResponse};
use gents::{
    ActiveRuntimeSnapshot, AgentRequest, ConcurrencyMode, DefraWatcher, EventSource,
    ResolvedEventTrigger, ResolvedTask, TriggerSource, Watcher,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::support::mock_subscription::MockUpdateSubscriptionSource;

const EVENT_SOURCE_COLLECTION: &str = "EventDeliveryDoc";
const EVENT_SOURCE_TRIGGER_ID: &str = "event-delivery-trigger";
const EVENT_SOURCE_TASK_ID: &str = "event-delivery-task";
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(2);
const RESCAN_TEST_INTERVAL: Duration = Duration::from_millis(50);

pub(super) async fn event_delivery_transition_cases_match_contract() {
    // These rows describe substrate bookkeeping, not a source-owner observation.
    // The ledger retains their missing runtime coverage; do not replay a second World.
    const UNOBSERVED: [&str; 8] = [
        "persist_into_empty",
        "persist_extends_set",
        "depersist_removes",
        "enqueue_from_persistent",
        "drop_from_queue",
        "deliver_consumes_queue",
        "rescan_on_empty",
        "enqueue_twice_multiset",
    ];
    let mut skipped = Vec::new();
    let mut observed = 0;
    let mut siblings = 0;
    let mut releases = 0;
    for case in lean_event_delivery_transition_cases() {
        if UNOBSERVED.contains(&case.name.as_str()) {
            skipped.push(case.name.as_str());
            continue;
        }
        if case.name == "handle_ready_trigger_preserves_pending_sibling" {
            siblings += 1; // Driven by the dedicated two-trigger EventSource test.
            continue;
        }
        if matches!(&case.action, LeanEventDeliveryAction::Release { .. }) {
            // Private cooldown prestate is hydrated by the native owner test
            // watcher::tests::generated_release_cases_preserve_native_session_order.
            releases += 1;
            continue;
        }
        let mut runtime = ProductionEventDeliveryDriver::new(
            runtime_event_delivery_source_contract("Watcher"),
            &case.pre,
        )
        .await;
        match &case.action {
            LeanEventDeliveryAction::RescanTick => {
                let emitted = case
                    .post
                    .subscription_queue
                    .strip_suffix(case.pre.subscription_queue.as_slice())
                    .expect("rescan witness retains its existing queue suffix");
                assert!(!emitted.is_empty(), "{} needs a real emission", case.name);
                runtime.drive_rescan(emitted).await.unwrap();
            }
            LeanEventDeliveryAction::Handle { doc } => {
                let emitted = runtime.drive_handle(doc).await.unwrap();
                assert_eq!(case.post.handled, vec![emitted], "{}", case.name);
            }
            other => panic!("{} needs an owner adapter for {other:?}", case.name),
        }
        observed += 1;
    }
    skipped.sort_unstable();
    let mut expected = UNOBSERVED.to_vec();
    expected.sort_unstable();
    assert_eq!(skipped, expected);
    assert_eq!(observed, 5);
    assert_eq!(releases, 2);
    assert_eq!(siblings, 1);
}

pub(super) fn event_delivery_source_instances_match_runtime() {
    let runtime_by_name = runtime_event_delivery_source_contracts()
        .iter()
        .map(|instance| (instance.name, *instance))
        .collect::<HashMap<_, _>>();

    for lean in lean_event_delivery_source_instances() {
        let runtime = runtime_by_name
            .get(lean.name.as_str())
            .unwrap_or_else(|| panic!("runtime source {:?} must be present", lean.name));
        assert_eq!(runtime.dedupe_policy, lean.dedupe_policy);
        assert_eq!(runtime.rescan_bounded_by, lean.rescan_bounded_by);
        assert_eq!(runtime.deviation, lean.deviation.as_deref());
    }
    assert_eq!(
        runtime_by_name.len(),
        lean_event_delivery_source_instances().len(),
        "runtime source introspection should not expose unmodeled sources"
    );
}

pub(super) async fn event_delivery_convergence_traces_match_runtime_or_deviation() {
    let traces = lean_event_delivery_convergence_traces();
    assert!(
        traces.len() >= lean_event_delivery_source_instances().len(),
        "Expected at least one convergence trace per source"
    );

    for trace in traces {
        let source = runtime_event_delivery_source_contract(&trace.instance_name);
        let mut runtime = ProductionEventDeliveryDriver::new(source, &trace.initial_world).await;
        let mut pending = trace
            .initial_world
            .persistent_set
            .iter()
            .filter(|doc| !trace.initial_world.processed_set.contains(*doc))
            .cloned()
            .collect::<Vec<_>>();
        let mut handled = Vec::new();
        for action in &trace.actions {
            match action {
                LeanEventDeliveryAction::Persist { doc } => {
                    // The mock bus receives no update here: delivery must come
                    // from the real source's rescan of persisted documents.
                    runtime
                        .persist_runtime_doc(doc, pending.len())
                        .await
                        .unwrap();
                    pending.push(doc.clone());
                }
                LeanEventDeliveryAction::RescanTick => {
                    assert!(
                        !pending.is_empty(),
                        "{} needs a rescan emission",
                        trace.name
                    );
                    runtime.drive_rescan(&pending).await.unwrap();
                }
                LeanEventDeliveryAction::Handle { doc } => {
                    let emitted = runtime.drive_handle(doc).await.unwrap();
                    handled.insert(0, emitted);
                    pending.retain(|pending| pending != doc);
                }
                other => panic!("{} needs an owner adapter for {other:?}", trace.name),
            }
        }
        assert_eq!(
            handled, trace.final_world.handled,
            "{} emitted requests",
            trace.name
        );

        match trace.status.as_str() {
            "substantive" => {
                assert!(
                    source.deviation.is_none(),
                    "substantive trace `{}` should run against a non-deviation source",
                    trace.name
                );
            }
            "deviation" => panic!(
                "event-delivery deviation trace `{}` is retired; live sources must emit substantive convergence traces",
                trace.name,
            ),
            other => panic!(
                "trace `{}` has unknown status `{}` (expected 'substantive' or 'deviation')",
                trace.name, other,
            ),
        }
    }

    let trace_instances: std::collections::HashSet<&str> =
        traces.iter().map(|t| t.instance_name.as_str()).collect();
    for name in lean_event_delivery_source_instances()
        .iter()
        .map(|instance| instance.name.as_str())
    {
        assert!(
            trace_instances.contains(name),
            "Expected a convergence trace for instance `{}`",
            name
        );
    }
}

struct ProductionEventDeliveryDriver {
    source: EventDeliverySourceContract,
    db: super::support::TestDb,
    mock_subs: MockUpdateSubscriptionSource,
    runtime: ProductionRuntime,
    cancel: CancellationToken,
    _snapshot_tx: Option<watch::Sender<Arc<ActiveRuntimeSnapshot>>>,
    runner: Option<tokio::task::JoinHandle<()>>,
    emitted_rx: Option<mpsc::Receiver<String>>,
    emitted_buffer: Vec<String>,
    doc_ids: HashMap<String, String>,
}

enum ProductionRuntime {
    Watcher { watcher: DefraWatcher },
    EventSource,
}

impl ProductionEventDeliveryDriver {
    async fn new(
        source: EventDeliverySourceContract,
        world: &lean_vocab_test::LeanEventDeliveryWorld,
    ) -> Self {
        let db = test_db(&format!("event-delivery-{}", source.name)).await;
        let mock_subs = MockUpdateSubscriptionSource::new();
        let cancel = CancellationToken::new();
        let mut driver = match source.name {
            "Watcher" => {
                let watcher = DefraWatcher::with_subscription_source(
                    Arc::new(mock_subs.clone()),
                    db.node.clone(),
                    NODE_DID,
                );
                Self {
                    source,
                    db,
                    mock_subs,
                    runtime: ProductionRuntime::Watcher { watcher },
                    cancel,
                    _snapshot_tx: None,
                    runner: None,
                    emitted_rx: None,
                    emitted_buffer: Vec::new(),
                    doc_ids: HashMap::new(),
                }
            }
            "EventSource" => {
                install_event_delivery_source_schema(db.node.as_ref()).await;
                let (runner, emitted_rx, snapshot_tx) =
                    spawn_event_source_runner(db.node.clone(), mock_subs.clone(), cancel.clone())
                        .await;
                assert!(
                    mock_subs
                        .wait_for_subscribers(1, Duration::from_secs(2))
                        .await,
                    "EventSource runner did not open its mock subscription"
                );
                Self {
                    source,
                    db,
                    mock_subs,
                    runtime: ProductionRuntime::EventSource,
                    cancel,
                    _snapshot_tx: Some(snapshot_tx),
                    runner: Some(runner),
                    emitted_rx: Some(emitted_rx),
                    emitted_buffer: Vec::new(),
                    doc_ids: HashMap::new(),
                }
            }
            other => panic!("unhandled event-delivery source {other:?}"),
        };
        driver.seed_world(world).await.unwrap_or_else(|err| {
            panic!(
                "failed to seed production event-delivery world for {}: {err}",
                driver.source.name
            )
        });
        driver
    }

    async fn seed_world(
        &mut self,
        world: &lean_vocab_test::LeanEventDeliveryWorld,
    ) -> Result<(), String> {
        for (index, doc) in world.persistent_set.iter().enumerate() {
            self.persist_runtime_doc(doc, index).await?;
            if world.processed_set.contains(doc) {
                // Poll through the owner to seed Watcher cooldown. Keep the
                // request pending; marking it completed would test a different filter.
                // Current witnesses put processed documents first in FIFO order.
                match &mut self.runtime {
                    ProductionRuntime::Watcher { watcher } => {
                        let request = poll_watcher(watcher).await?;
                        if request.request_id != *doc {
                            return Err(format!(
                                "cooldown fixture emitted {:?}, expected {doc:?}",
                                request.request_id
                            ));
                        }
                    }
                    _ => return Err("processed seed needs a source-specific owner adapter".into()),
                }
            }
        }
        for doc in &world.subscription_queue {
            self.publish_update(doc)?;
        }
        Ok(())
    }

    async fn persist_runtime_doc(&mut self, doc: &str, sequence: usize) -> Result<(), String> {
        if self.doc_ids.contains_key(doc) {
            return Ok(());
        }
        let doc_id = match self.source.name {
            "Watcher" => {
                create_request(
                    self.db.node.as_ref(),
                    doc,
                    &format!("event-delivery-session-{}", sanitize_graphql_id(doc)),
                    "pending",
                    &format!("2026-05-20T00:00:{:02}Z", sequence % 60),
                )
                .await
            }
            "EventSource" => self.create_event_delivery_doc(doc).await?,
            other => return Err(format!("unsupported source {other:?}")),
        };
        self.doc_ids.insert(doc.to_string(), doc_id);
        Ok(())
    }

    async fn create_event_delivery_doc(&self, doc: &str) -> Result<String, String> {
        let external_id = escape_graphql_string(doc);
        let mutation = format!(
            r#"mutation {{
                add_{EVENT_SOURCE_COLLECTION}(input: {{
                    external_id: "{external_id}",
                    payload: "{{}}"
                }}) {{ _docID }}
            }}"#
        );
        let resp = self.db.node.execute(&mutation).await;
        if resp.has_errors() {
            return Err(format!(
                "add_{EVENT_SOURCE_COLLECTION} failed: {:?}",
                resp.errors
            ));
        }
        mutation_doc_id(&resp, &format!("add_{EVENT_SOURCE_COLLECTION}"))
            .ok_or_else(|| format!("add_{EVENT_SOURCE_COLLECTION} returned no _docID"))
    }

    fn publish_update(&self, doc: &str) -> Result<(), String> {
        let collection = match self.source.name {
            "Watcher" => "AgentRequest",
            "EventSource" => EVENT_SOURCE_COLLECTION,
            other => return Err(format!("unsupported source {other:?}")),
        };
        let collection_id = self.collection_id(collection)?;
        let doc_id = self
            .doc_ids
            .get(doc)
            .cloned()
            .unwrap_or_else(|| doc.to_string());
        self.mock_subs.publish_update(collection_id, doc_id);
        Ok(())
    }

    fn collection_id(&self, collection: &str) -> Result<String, String> {
        self.db
            .node
            .get_collection(collection)
            .map_err(|err| format!("get_collection({collection}) failed: {err}"))?
            .map(|definition| definition.collection_id)
            .ok_or_else(|| format!("collection {collection:?} not found"))
    }

    async fn drive_rescan(&mut self, expected_docs: &[String]) -> Result<(), String> {
        match &mut self.runtime {
            ProductionRuntime::Watcher { watcher } => {
                for expected in expected_docs {
                    let request = poll_watcher(watcher).await?;
                    if request.request_id != *expected {
                        return Err(format!(
                            "watcher rescan emitted {:?}, expected {:?}",
                            request.request_id, expected
                        ));
                    }
                    self.emitted_buffer.push(request.request_id);
                }
                Ok(())
            }
            ProductionRuntime::EventSource => {
                for expected in expected_docs {
                    let expected = self.production_doc_id(expected)?;
                    self.wait_for_emitted_doc_buffered(&expected, DELIVERY_TIMEOUT)
                        .await?;
                }
                Ok(())
            }
        }
    }

    async fn drive_handle(&mut self, doc: &str) -> Result<String, String> {
        match &mut self.runtime {
            ProductionRuntime::Watcher { watcher } => {
                if let Some(emitted) = erase_first(&mut self.emitted_buffer, doc) {
                    return Ok(emitted);
                }
                let request = poll_watcher(watcher).await?;
                if request.request_id != doc {
                    return Err(format!(
                        "watcher handle emitted {:?}, expected {:?}",
                        request.request_id, doc
                    ));
                }
                Ok(request.request_id)
            }
            ProductionRuntime::EventSource => {
                let expected = self.production_doc_id(doc)?;
                self.wait_for_emitted_doc(&expected, DELIVERY_TIMEOUT)
                    .await?;
                // The emitted physical document id was checked above; return
                // its logical fixture id for comparison with the generated row.
                Ok(doc.to_string())
            }
        }
    }

    fn production_doc_id(&self, doc: &str) -> Result<String, String> {
        match self.source.name {
            "Watcher" => Ok(doc.to_string()),
            "EventSource" => self
                .doc_ids
                .get(doc)
                .cloned()
                .ok_or_else(|| format!("doc {doc:?} has no runtime row")),
            other => Err(format!("unsupported source {other:?}")),
        }
    }

    async fn wait_for_emitted_doc(
        &mut self,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        if erase_first(&mut self.emitted_buffer, expected).is_some() {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.wait_for_any_emitted_doc_until(deadline).await? {
                Some(emitted) if emitted == expected => return Ok(()),
                Some(emitted) => self.emitted_buffer.push(emitted),
                None => {
                    return Err(format!(
                        "{} did not emit expected runtime doc {:?}",
                        self.source.name, expected
                    ));
                }
            }
        }
    }

    async fn wait_for_emitted_doc_buffered(
        &mut self,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        if self.emitted_buffer.iter().any(|doc| doc == expected) {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.wait_for_any_emitted_doc_until(deadline).await? {
                Some(emitted) => {
                    let matched = emitted == expected;
                    self.emitted_buffer.push(emitted);
                    if matched {
                        return Ok(());
                    }
                }
                None => {
                    return Err(format!(
                        "{} did not emit expected runtime doc {:?}",
                        self.source.name, expected
                    ));
                }
            }
        }
    }

    async fn wait_for_any_emitted_doc_until(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<Option<String>, String> {
        let Some(rx) = &mut self.emitted_rx else {
            return Ok(None);
        };
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(doc)) => Ok(Some(doc)),
            Ok(None) => Err(format!("{} runner exited", self.source.name)),
            Err(_) => Ok(None),
        }
    }
}

impl Drop for ProductionEventDeliveryDriver {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(runner) = &self.runner {
            runner.abort();
        }
    }
}

async fn spawn_event_source_runner(
    node: Arc<EmbeddedNode>,
    mock_subs: MockUpdateSubscriptionSource,
    cancel: CancellationToken,
) -> (
    tokio::task::JoinHandle<()>,
    mpsc::Receiver<String>,
    watch::Sender<Arc<ActiveRuntimeSnapshot>>,
) {
    let access = gents::config_client::ConfigAccess::Local(node.clone());
    let trigger_doc_id = install_event_delivery_config(&access).await;
    let mut snapshot = active_snapshot_with_event_trigger();
    Arc::make_mut(&mut snapshot)
        .active_event_triggers
        .get_mut(EVENT_SOURCE_TRIGGER_ID)
        .unwrap()
        .trigger_doc_id = trigger_doc_id.clone();
    let (snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let mut source =
        EventSource::with_subscription_source(Arc::new(mock_subs), snapshot_rx, node, cancel)
            .with_rescan_interval(RESCAN_TEST_INTERVAL);
    let (tx, rx) = mpsc::channel(16);
    let runner = tokio::spawn(async move {
        while let Some(mut intent) = source.next_fire().await {
            let doc_id = intent.event_vars["source_doc_id"]
                .as_str()
                .expect("source document identity")
                .to_owned();
            let admission = admit_event_delivery(&access, &mut intent, &trigger_doc_id, &doc_id)
                .await
                .expect("admit source fire through Task delivery owner");
            let result = if admission.duplicate {
                gents::FireResult::Duplicate {
                    request_id: admission.request.request_id,
                }
            } else {
                gents::FireResult::Fired {
                    request_id: admission.request.request_id,
                }
            };
            (intent.on_result)(result);
            if tx.send(doc_id).await.is_err() {
                break;
            }
        }
    });
    (runner, rx, snapshot_tx)
}

async fn install_event_delivery_config(access: &gents::config_client::ConfigAccess) -> String {
    use gents::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::Collection;
    use serde_json::json;
    let documents = [
        (Collection::InferenceBackend, json!({"backend_id":"event-backend", "name":"Test", "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:8000/v1", "auth":{"kind":"unauthenticated"}})),
        (Collection::InferenceProfile, json!({"profile_id":"event-profile", "backend_id":"event-backend", "model_name":"model"})),
        (Collection::Agent, json!({"agent_id":AGENT_NAME, "inference_profile_id":"event-profile"})),
        (Collection::Task, json!({"task_id":EVENT_SOURCE_TASK_ID, "agent_id":AGENT_NAME, "prompt_template":"handle event delivery doc"})),
        (Collection::EventSource, json!({"event_source_id":"event-delivery-source", "source_collection":EVENT_SOURCE_COLLECTION, "event_kind":"created"})),
        (Collection::Trigger, json!({"trigger_id":EVENT_SOURCE_TRIGGER_ID, "task_id":EVENT_SOURCE_TASK_ID, "enabled":true, "concurrency":"queued_serial", "source":{"kind":"event", "event_source_id":"event-delivery-source"}})),
    ].into_iter().map(|(collection, mut value)| {
        value["node_did"] = json!(NODE_DID);
        DesiredStateApplyDocument { collection, add:value.clone(), update:value }
    }).collect();
    let plan = DesiredStateApplyPlan::new(documents).expect("event source configuration plan");
    access.transact("test.event_delivery.configure", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            let query = format!("{{ Trigger(filter: {{node_did: {{_eq: \"{}\"}}, trigger_id: {{_eq: \"{}\"}}}}) {{_docID}} }}",
                escape_graphql_string(NODE_DID), escape_graphql_string(EVENT_SOURCE_TRIGGER_ID));
            let response = txn.execute(&query).await?;
            Ok(response["data"]["Trigger"][0]["_docID"].as_str()
                .expect("persisted trigger physical identity").to_owned())
        })
    }).await.expect("persist event source configuration")
}

async fn admit_event_delivery(
    access: &gents::config_client::ConfigAccess,
    intent: &mut gents::FireIntent,
    trigger_doc_id: &str,
    doc_id: &str,
) -> anyhow::Result<gents::lifecycle::TaskDeliveryAdmission> {
    use gents::lifecycle::{
        build_signed_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
        TriggerLineage,
    };
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
    let identity = FireIdentity {
        owner_did: NODE_DID.into(),
        trigger_id: EVENT_SOURCE_TRIGGER_ID.into(),
        source_collection: EVENT_SOURCE_COLLECTION.into(),
        source_doc_id: doc_id.into(),
    };
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let fire = TriggerFire {
        fire_key: identity.fire_key(),
        request_id: identity.request_id(),
        session_id: identity.session_id(),
        identity,
        task_id: EVENT_SOURCE_TASK_ID.into(),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: true,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: now.clone(),
    };
    let mut spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            requester_did: None,
            request_id: fire.request_id.clone(),
            node_did: NODE_DID.into(),
            agent_id: AGENT_NAME.into(),
            session_id: fire.session_id.clone(),
            content: intent.task.prompt_template.clone(),
            execution_origin: ExecutionOrigin::Scheduled,
            created_at: now,
        },
        AgentRequestAdmissionRecord::runtime_automated_trigger(NODE_DID, EVENT_SOURCE_TRIGGER_ID),
    );
    spec.trigger_lineage = TriggerLineage {
        trigger_id: Some(EVENT_SOURCE_TRIGGER_ID.into()),
        trigger_kind: Some("event".into()),
        source_doc_id: Some(doc_id.into()),
        correlation: None,
        trigger_context: None,
    };
    spec.trigger_doc_id = Some(trigger_doc_id.into());
    let signer = crate::support::materialization_identity();
    let create = build_signed_request(spec, RequestSigner::Identity(signer.as_ref())).await?;
    gents::lifecycle::write_task_delivery(access, &fire, false, &create).await
}

async fn poll_watcher(watcher: &mut DefraWatcher) -> Result<AgentRequest, String> {
    tokio::time::timeout(DELIVERY_TIMEOUT, watcher.next_request())
        .await
        .map_err(|_| "watcher timed out waiting for AgentRequest".to_string())?
        .ok_or_else(|| "watcher exhausted before emitting AgentRequest".to_string())?
        .map_err(|err| format!("watcher returned error: {err}"))
}

async fn install_event_delivery_source_schema(node: &EmbeddedNode) {
    let schema = r#"
        type EventDeliveryDoc {
            external_id: String @index
            payload: String
        }
    "#;
    node.add_schema(schema)
        .await
        .expect("add_schema for EventDeliveryDoc");
}

fn active_snapshot_with_event_trigger() -> Arc<ActiveRuntimeSnapshot> {
    let task = ResolvedTask {
        emit_outcome: false,
        task_id: EVENT_SOURCE_TASK_ID.to_string(),
        name: Some(EVENT_SOURCE_TASK_ID.to_string()),
        agent_id: AGENT_NAME.to_string(),
        prompt_template: "handle event delivery doc".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let trigger = ResolvedEventTrigger {
        session_id_template: None,
        trigger_doc_id: "event-source-trigger-doc".to_string(),
        trigger_id: EVENT_SOURCE_TRIGGER_ID.to_string(),
        task_id: task.task_id.clone(),
        task: task.clone(),
        source_collection: EVENT_SOURCE_COLLECTION.to_string(),
        event_kind: "created".to_string(),
        filter: None,
        enabled: true,
        concurrency: ConcurrencyMode::QueuedSerial,
        fire_mode: gents::EventTriggerFireMode::PerDocument,
        correlation_field: None,
        expected_count: None,
        expected_count_field: None,
        group_timeout_secs: None,
        group_min_count: 1,
        workspace_authority: None,
    };
    active_snapshot(
        HashMap::from([(trigger.trigger_id.clone(), trigger)]),
        HashMap::from([(task.task_id.clone(), task)]),
    )
}

fn active_snapshot(
    active_event_triggers: HashMap<String, ResolvedEventTrigger>,
    active_tasks: HashMap<String, ResolvedTask>,
) -> Arc<ActiveRuntimeSnapshot> {
    Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: NODE_DID.to_string(),
        default_agent_id: AGENT_NAME.to_string(),
        agents: HashMap::from([(AGENT_NAME.to_string(), runtime_agent(AGENT_NAME))]),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_agents: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers,
        unavailable_event_triggers: HashSet::new(),
        active_tasks,
        dispatchers: HashMap::new(),
        agent_executor_capacities: HashMap::new(),
        agent_executor_queue_capacities: HashMap::new(),
    })
}

fn runtime_agent(agent_id: &str) -> Arc<gents::ResolvedAgent> {
    let identity: Arc<dyn gents::NodeIdentity> = Arc::new(crate::support::fixtures::test_identity(
        &format!("event-delivery-{agent_id}"),
    ));
    let node = Arc::new(gents::RuntimeNode {
        node_did: NODE_DID.to_string(),
        identity,
        default_agent_id: AGENT_NAME.to_string(),
        display_name: None,
        enabled: true,
    });
    Arc::new(crate::support::fixtures::test_agent_for_node(
        agent_id, node,
    ))
}

fn sanitize_graphql_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn mutation_doc_id(resp: &QueryResponse, field: &str) -> Option<String> {
    let value = resp.data.as_ref()?.get(field)?;
    value
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn erase_first(values: &mut Vec<String>, target: &str) -> Option<String> {
    values
        .iter()
        .position(|value| value == target)
        .map(|index| values.remove(index))
}

fn runtime_event_delivery_source_contract(name: &str) -> EventDeliverySourceContract {
    runtime_event_delivery_source_contracts()
        .into_iter()
        .find(|source| source.name == name)
        .unwrap_or_else(|| panic!("runtime event-delivery source {name:?} must be present"))
}
