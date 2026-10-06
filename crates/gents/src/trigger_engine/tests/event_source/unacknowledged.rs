//! #2094: an arrival whose fire is not acknowledged stays pending, and its
//! own `Trigger` runtime-field write must not re-drive it without bound.

use super::*;

const RESCAN: Duration = Duration::from_millis(50);

struct Delivery {
    node: Arc<defra_node::EmbeddedNode>,
    snapshot_tx: watch::Sender<Arc<ActiveRuntimeSnapshot>>,
    source: EventSource,
    engine: TriggerEngine,
    doc_id: String,
}

fn ping_trigger(emit_outcome: bool) -> ResolvedEventTrigger {
    let task = ResolvedTask {
        emit_outcome,
        task_id: "ping-task".to_string(),
        ..resolved_task("Handle the ping: {{ doc.message }}")
    };
    ResolvedEventTrigger {
        concurrency: ConcurrencyMode::Parallel,
        ..resolved_event_trigger("ping-trigger", "OutcomePing", task)
    }
}

fn ping_snapshot(generation: u64, emit_outcome: bool) -> Arc<ActiveRuntimeSnapshot> {
    snapshot_with_event_triggers(
        generation,
        HashMap::from([("ping-trigger".to_string(), ping_trigger(emit_outcome))]),
    )
}

/// One `OutcomePing` without `handoff_id`, created after the trigger's cursor
/// is seeded, delivered once through the durable arrival path.
async fn first_delivery(emit_outcome: bool) -> (Delivery, FireIntent) {
    first_delivery_on("type OutcomePing { message: String }", emit_outcome).await
}

async fn first_delivery_on(schema: &str, emit_outcome: bool) -> (Delivery, FireIntent) {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node.add_schema(schema).await.unwrap();
    let snapshot = ping_snapshot(1, emit_outcome);
    persist_event_bindings(&node, snapshot.as_ref()).await;
    let (snapshot_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx.clone(), node.clone(), CancellationToken::new())
        .with_rescan_interval(RESCAN);
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let writer = node.clone();
    let created = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let response = crate::config_client::ConfigAccess::Local(writer)
            .write(
                "test.outcome_ping",
                r#"mutation { create_OutcomePing(input: {message: "train-echo-11"}) { _docID } }"#,
            )
            .await
            .unwrap();
        crate::graphql::created_doc_id(&response, "OutcomePing").unwrap()
    });
    let intent = tokio::time::timeout(Duration::from_secs(5), source.next_fire())
        .await
        .expect("the seeded document was never delivered")
        .expect("source closed");
    let doc_id = created.await.unwrap();
    assert_eq!(
        intent.event_vars["source_doc_id"].as_str(),
        Some(doc_id.as_str())
    );
    let engine = TriggerEngine::new(rx, SpyMaterializer::new());
    (
        Delivery {
            node,
            snapshot_tx,
            source,
            engine,
            doc_id,
        },
        intent,
    )
}

impl Delivery {
    async fn cursor(&self) -> String {
        let owner = event_test_behavior().agent_did().to_owned();
        crate::config_client::ConfigAccess::Local(self.node.clone())
            .transact("test.unacknowledged_cursor", |txn| {
                let owner = &owner;
                Box::pin(async move {
                    Ok(
                        crate::config_client::event_source_cursor::load_or_seed_for_source(
                            txn,
                            owner,
                            "ping-trigger",
                            "OutcomePing",
                        )
                        .await?
                        .cursor
                        .after,
                    )
                })
            })
            .await
            .unwrap()
    }

    async fn pending_documents(&self) -> Vec<String> {
        let after = self.cursor().await;
        let page = self
            .node
            .execute(&format!(
                "{{ _documentArrivals(collection: \"OutcomePing\", after: \"{}\", limit: 128) {{ entries {{ docID }} }} }}",
                escape_graphql_string(&after),
            ))
            .await;
        page.data.unwrap()["_documentArrivals"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["docID"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Every intent the source emits within `window`, each answered by `answer`.
    async fn drain(
        &mut self,
        window: Duration,
        answer: impl Fn(&FireIntent) -> Option<FireResult>,
    ) -> usize {
        let deadline = tokio::time::Instant::now() + window;
        let mut emitted = 0;
        while let Ok(Some(intent)) =
            tokio::time::timeout_at(deadline, self.source.next_fire()).await
        {
            emitted += 1;
            match answer(&intent) {
                Some(result) => (intent.on_result)(result),
                None => {
                    self.engine.dispatch(intent).await;
                }
            }
        }
        emitted
    }

    async fn trigger_status(&self, status: &str) -> serde_json::Value {
        for _ in 0..100 {
            let trigger = observed_trigger(self.node.as_ref(), "ping-trigger").await;
            if trigger["last_status"].as_str() == Some(status) {
                return trigger;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the fire never became visible as {status} on its Trigger");
    }

    async fn trigger_error(&self) -> serde_json::Value {
        self.trigger_status("error").await
    }
}

fn lean_cursor_case(name: &str) -> serde_json::Value {
    gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap()["trigger_delivery"]
        ["cursors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("Lean cursor case {name} is missing"))
        .clone()
}

/// The L4 ladder's Task opts into `emit_outcome` for a collection with no
/// `handoff_id`. Admission refuses the fire once, visibly on the Trigger, and
/// the arrival stays pending (Lean `unadmitted_match_cannot_checkpoint`)
/// without being re-driven by its own bookkeeping write or the rescan tick.
/// A configuration change delivers it exactly once.
#[tokio::test]
async fn refused_fire_fails_once_visibly_and_is_delivered_once_after_reconfiguration() {
    let model = lean_cursor_case("unadmitted_match_cannot_checkpoint");
    assert_eq!(model["post_cursor"], model["pre_cursor"]);
    assert!(model["journal_after"]
        .as_array()
        .unwrap()
        .contains(&model["entry"]));
    assert!(model["post"]["receipts"].as_array().unwrap().is_empty());

    let (mut delivery, intent) = first_delivery(true).await;
    let before = delivery.cursor().await;
    match delivery.engine.dispatch(intent).await {
        FireResult::Rejected { error } => assert!(
            error.contains("emit_outcome requires a source handoff_id"),
            "{error}"
        ),
        other => panic!("expected the fire to be refused, got {other:?}"),
    }
    let repeats = delivery.drain(Duration::from_secs(2), |_| None).await;
    assert_eq!(
        repeats, 0,
        "the refused arrival was re-driven {repeats} times"
    );

    let trigger = delivery.trigger_error().await;
    assert!(
        trigger["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("handoff_id")),
        "{trigger}"
    );
    assert_eq!(trigger["fire_count"].as_i64().unwrap_or(0), 0);
    assert_eq!(delivery.cursor().await, before);
    assert_eq!(
        delivery.pending_documents().await,
        vec![delivery.doc_id.clone()]
    );

    delivery.snapshot_tx.send(ping_snapshot(2, false)).unwrap();
    let node = delivery.node.clone();
    let intent = tokio::time::timeout(Duration::from_secs(5), delivery.source.next_fire())
        .await
        .expect("the pending arrival was not retried after the configuration changed")
        .expect("source closed");
    assert_eq!(
        intent.event_vars["source_doc_id"].as_str(),
        Some(delivery.doc_id.as_str())
    );
    let result = admit_observed_event(&node, &intent).await;
    (intent.on_result)(result);
    let repeats = delivery.drain(Duration::from_secs(1), |_| None).await;
    assert_eq!(repeats, 0, "an admitted arrival fired again");
    assert_ne!(delivery.cursor().await, before);
    assert!(delivery.pending_documents().await.is_empty());
    let receipts = node
        .execute("{ TriggerFire { fire_key } }")
        .await
        .data
        .unwrap();
    assert_eq!(receipts["TriggerFire"].as_array().unwrap().len(), 1);
}

/// A transient failure keeps the arrival pending and is retried, but on a
/// capped exponential backoff rather than on every source event.
#[tokio::test]
async fn transient_fire_failure_retries_on_a_bounded_backoff() {
    let (mut delivery, intent) = first_delivery(false).await;
    let before = delivery.cursor().await;
    let transient = || FireResult::Errored {
        error: "injected transient admission failure".into(),
    };
    (intent.on_result)(transient());
    // Backoff from a 50ms interval retries at about 50, 150, 350, 750 and
    // 1550ms; an unbounded re-drive would fire once per runtime-field write.
    let retries = delivery
        .drain(Duration::from_secs(2), |_| Some(transient()))
        .await;
    assert!((2..=6).contains(&retries), "retried {retries} times");
    delivery.trigger_error().await;
    assert_eq!(delivery.cursor().await, before);
    assert_eq!(
        delivery.pending_documents().await,
        vec![delivery.doc_id.clone()]
    );
}

/// A fire whose result never reaches the acknowledgment channel records the
/// reason on its `Trigger` instead of parking silently, keeps the arrival
/// pending, and is still retried only on the capped backoff — its own
/// `Trigger` write must not re-drive it without bound.
#[tokio::test]
async fn an_unacknowledged_fire_records_its_reason_on_the_trigger() {
    let (mut delivery, intent) = first_delivery(false).await;
    let before = delivery.cursor().await;
    // The guard the intent carries owns the write. Dropping the intent
    // unacknowledged is the state a driver that dies before any result
    // leaves behind, so the reason is recorded without the source ever
    // being polled again.
    drop(intent);
    let trigger = delivery
        .trigger_status(crate::trigger_engine::UNACKNOWLEDGED_STATUS)
        .await;
    assert!(
        trigger["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("acknowledgment channel closed")),
        "{trigger}"
    );
    assert_eq!(trigger["fire_count"].as_i64().unwrap_or(0), 0);
    assert_eq!(
        trigger["last_fired_source_doc_id"].as_str(),
        Some(delivery.doc_id.as_str()),
        "{trigger}"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut redrives = 0;
    while let Ok(Some(retried)) =
        tokio::time::timeout_at(deadline, delivery.source.next_fire()).await
    {
        redrives += 1;
        drop(retried);
    }
    assert!(
        (1..=8).contains(&redrives),
        "the unacknowledged arrival was re-driven {redrives} times"
    );
    assert_eq!(delivery.cursor().await, before);
    assert_eq!(
        delivery.pending_documents().await,
        vec![delivery.doc_id.clone()]
    );
}

/// A driver that dies between emitting the intent and seeing it answered —
/// here a panic inside dispatch — is the loss mode no live owner can report;
/// the guard dropped during the unwind still records the fire's reason.
#[tokio::test]
async fn a_driver_that_dies_mid_dispatch_records_the_fire_as_unacknowledged() {
    let (delivery, intent) = first_delivery(false).await;
    let engine = TriggerEngine::new(
        delivery.snapshot_tx.subscribe(),
        Arc::new(PanicMaterializer),
    );
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let driver = tokio::spawn(async move {
        engine.dispatch(intent).await;
    });
    let joined = driver.await;
    std::panic::set_hook(previous_hook);
    joined.expect_err("the materializer must have killed the driver");
    let trigger = delivery
        .trigger_status(crate::trigger_engine::UNACKNOWLEDGED_STATUS)
        .await;
    assert_eq!(
        trigger["last_fired_source_doc_id"].as_str(),
        Some(delivery.doc_id.as_str()),
        "{trigger}"
    );
}

/// A fire that is acknowledged disarms the guard: its `on_result` writer is
/// the only `Trigger` write the fire produces.
#[tokio::test]
async fn an_acknowledged_fire_records_no_unacknowledged_status() {
    let (delivery, intent) = first_delivery(false).await;
    (intent.on_result)(FireResult::Errored {
        error: "acknowledged transient failure".into(),
    });
    let trigger = delivery.trigger_status("error").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let settled = observed_trigger(delivery.node.as_ref(), "ping-trigger").await;
    assert_ne!(
        settled["last_status"].as_str(),
        Some(crate::trigger_engine::UNACKNOWLEDGED_STATUS),
        "{settled}"
    );
    assert!(
        trigger["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("acknowledged transient failure")),
        "{trigger}"
    );
}

/// Kills the driver inside dispatch, the one production loss mode the guard
/// exists for.
struct PanicMaterializer;

impl MaterializerHandle for PanicMaterializer {
    fn materialize(
        &self,
        _task: &crate::runtime_snapshot::ResolvedTask,
        _trigger_id: Option<&str>,
        _trigger_kind: TriggerKind,
        _trigger_doc_id: Option<&str>,
        _source_doc_id: Option<&str>,
        _correlation: Option<&str>,
        _trigger_context: Option<&str>,
        _rendered_prompt: &str,
        _rendered_goal_objective: Option<&str>,
        _durable_fire_key: &str,
        _delivery: Option<&crate::trigger_engine::durable::PreparedFire>,
        _prepared_ids: Option<(&str, &str)>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + '_>> {
        Box::pin(async { panic!("injected materializer panic") })
    }

    fn has_active_runtime_request_for_trigger(
        &self,
        _agent_did: &str,
        _trigger_id: &str,
        _excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
        Box::pin(async { Ok(false) })
    }

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        _agent_did: &str,
        _trigger_id: &str,
        _excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<usize>> + Send + '_>> {
        Box::pin(async { Ok(0) })
    }

    fn recover_goal_task_fire(
        &self,
        _task: &crate::runtime_snapshot::ResolvedTask,
        _durable_fire_key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + Send + '_>> {
        Box::pin(async { Ok(None) })
    }

    fn has_materialized_group_request(
        &self,
        _agent_did: &str,
        _trigger_id: &str,
        _durable_fire_key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
        Box::pin(async { Ok(false) })
    }
}

/// A refusal decided by the source document is retried when that document is
/// repaired, without a configuration change, and delivered exactly once.
#[tokio::test]
async fn refused_fire_is_retried_once_when_its_source_document_is_repaired() {
    repaired_document_is_delivered_once(false).await;
}

/// The same repair is found by the rescan when its notification is lost.
#[tokio::test]
async fn refused_fire_repair_is_found_without_its_notification() {
    repaired_document_is_delivered_once(true).await;
}

async fn repaired_document_is_delivered_once(drop_notification: bool) {
    let (mut delivery, intent) = first_delivery_on(
        "type OutcomePing { message: String handoff_id: String }",
        true,
    )
    .await;
    assert!(matches!(
        delivery.engine.dispatch(intent).await,
        FireResult::Rejected { .. }
    ));
    assert_eq!(delivery.drain(Duration::from_secs(1), |_| None).await, 0);
    delivery.trigger_error().await;

    crate::config_client::ConfigAccess::Local(delivery.node.clone())
        .write(
            "test.repair_outcome_ping",
            &format!(
                r#"mutation {{ update_OutcomePing(docID: "{}", input: {{handoff_id: "handoff-1"}}) {{ _docID }} }}"#,
                escape_graphql_string(&delivery.doc_id)
            ),
        )
        .await
        .unwrap();
    if drop_notification {
        delivery.source.drop_subscription();
    }
    let node = delivery.node.clone();
    let intent = tokio::time::timeout(Duration::from_secs(5), delivery.source.next_fire())
        .await
        .expect("the repaired document was not retried")
        .expect("source closed");
    assert_eq!(intent.doc_vars.as_ref().unwrap()["handoff_id"], "handoff-1");
    let result = admit_observed_event(&node, &intent).await;
    (intent.on_result)(result);
    assert_eq!(delivery.drain(Duration::from_secs(1), |_| None).await, 0);
    assert!(delivery.pending_documents().await.is_empty());
}

/// A refused document deleted while notifications are lost releases its
/// trigger: the arrival owner excludes it without a fire.
#[tokio::test]
async fn deleted_refused_document_releases_its_trigger() {
    let (mut delivery, intent) = first_delivery(true).await;
    assert!(matches!(
        delivery.engine.dispatch(intent).await,
        FireResult::Rejected { .. }
    ));
    assert_eq!(delivery.drain(Duration::from_secs(1), |_| None).await, 0);
    crate::config_client::ConfigAccess::Local(delivery.node.clone())
        .write(
            "test.delete_outcome_ping",
            &format!(
                r#"mutation {{ delete_OutcomePing(docID: "{}") {{ _docID }} }}"#,
                escape_graphql_string(&delivery.doc_id)
            ),
        )
        .await
        .unwrap();
    delivery.source.drop_subscription();
    assert_eq!(delivery.drain(Duration::from_secs(2), |_| None).await, 0);
    assert!(delivery.pending_documents().await.is_empty());
}
