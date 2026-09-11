use super::*;

fn resolved_task_for_test(task_id: &str, behavior_id: &str, prompt_template: &str) -> ResolvedTask {
    ResolvedTask {
        task_id: task_id.to_string(),
        name: None,
        behavior_id: behavior_id.to_string(),
        prompt_template: prompt_template.to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    }
}

/// Build an `ActiveRuntimeSnapshot` with a single active task and no other
/// live state. Mirrors `snapshot_with_schedules`. Used by the manual-fire
/// tests that need `snapshot.active_tasks()` to resolve the intent's task.
fn snapshot_with_active_task(task: ResolvedTask) -> Arc<ActiveRuntimeSnapshot> {
    let mut tasks = HashMap::new();
    tasks.insert(task.task_id.clone(), task);
    let resolved = ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        "general".to_string(),
        vec![integration_test_behavior("general")],
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_automation(crate::runtime_snapshot::ResolvedAutomation {
        tasks,
        ..Default::default()
    })
    .with_principal(stub_principal());
    Arc::new(resolved.activate(1, HashMap::new()))
}

#[tokio::test]
async fn manual_source_run_task_now_yields_intent_with_args_vars() {
    let snapshot = snapshot_with_active_task(resolved_task_for_test(
        "greet-user",
        "behavior-1",
        "hello {{ args.name }}",
    ));
    let cancel = CancellationToken::new();
    let (mut source, handle) = ManualSource::new(cancel.clone());

    let pull = tokio::spawn(async move { source.next_fire().await });

    let _result_rx = handle
        .run_task_now(
            snapshot.as_ref(),
            "greet-user",
            serde_json::json!({"name": "Amy"}),
        )
        .await
        .unwrap();

    let intent = pull.await.unwrap().expect("next_fire returned None");
    assert_eq!(intent.trigger_kind, TriggerKind::Manual);
    assert_eq!(intent.trigger_id, None);
    assert_eq!(intent.concurrency, ConcurrencyMode::Parallel);
    assert_eq!(
        intent.args_vars.as_ref().and_then(|v| v["name"].as_str()),
        Some("Amy"),
    );
    assert_eq!(intent.task.task_id, "greet-user");
    assert_eq!(intent.event_vars["trigger_kind"].as_str(), Some("manual"));
    assert!(intent.doc_vars.is_none());
    let invocation_id = intent
        .durable_fire_key
        .strip_prefix("6:manual:36:")
        .expect("length-delimited manual fire key prefix");
    uuid::Uuid::parse_str(invocation_id).expect("manual invocation key must be a UUID");
}

#[tokio::test]
async fn manual_source_run_task_now_rejects_unknown_task() {
    let snapshot =
        snapshot_with_active_task(resolved_task_for_test("other-task", "behavior-1", "x"));
    let (_source, handle) = ManualSource::new(CancellationToken::new());
    let err = handle
        .run_task_now(snapshot.as_ref(), "missing", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not in the active snapshot"),
        "expected 'not in the active snapshot' in error, got: {err}"
    );
}

#[tokio::test]
async fn manual_source_next_fire_returns_none_after_cancel() {
    let cancel = CancellationToken::new();
    let (mut source, _handle) = ManualSource::new(cancel.clone());

    // Cancel immediately.
    cancel.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_millis(200), source.next_fire())
        .await
        .expect("timed out waiting for cancelled next_fire");
    assert!(result.is_none());
}

#[tokio::test]
async fn production_materializer_persists_event_source_document_lineage() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    let identity = behavior.principal_identity().clone();
    let owner = behavior.agent_did();
    crate::test_support::install_test_behavior(node.as_ref(), owner, "general").await;
    let config = serde_json::from_value(serde_json::json!({
        "agent_principal":{"agent_did":owner},
        "tasks":[{"agent_did":owner,"task_id":"task-event","behavior_id":"general","prompt_template":"event body"}],
        "event_sources":[{"agent_did":owner,"event_source_id":"source","source_collection":"AgentRequest","event_kind":"created"}],
        "triggers":[{"agent_did":owner,"trigger_id":"event-trigger","task_id":"task-event","source":{"kind":"event","event_source_id":"source"}}]
    })).unwrap();
    let plan = crate::config_client::DesiredStateApplyPlan::from_pack_config(&config).unwrap();
    let trigger_doc_id = crate::config_client::ConfigAccess::transact_local(
        node.as_ref(),
        None,
        "test.manual_source.fixture",
        |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::config_client::apply_desired_state_plan(txn, plan).await?;
                let (id, _) = crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Trigger,
                    owner,
                    "event-trigger",
                )
                .await?
                .expect("created trigger");
                Ok(id)
            })
        },
    )
    .await
    .unwrap();
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), rx);
    let task = resolved_task_for_test("task-event", "general", "event body");

    let request_id = materializer
        .materialize(
            &task,
            Some("event-trigger"),
            TriggerKind::Event,
            Some(&trigger_doc_id),
            Some("source-doc-exact"),
            None,
            None,
            "event body",
            None,
            "event-test-fire",
        )
        .await
        .expect("Event materialize should succeed");

    let escaped_request_id = escape_graphql_string(&request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ _docID caused_by_trigger_kind caused_by_trigger_doc_id caused_by_source_doc_id }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "AgentRequest query failed: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .expect("event AgentRequest row");
    assert_eq!(
        row.get("caused_by_trigger_kind")
            .and_then(serde_json::Value::as_str),
        Some("event")
    );
    assert_eq!(
        row.get("caused_by_trigger_doc_id")
            .and_then(serde_json::Value::as_str),
        Some(trigger_doc_id.as_str())
    );
    assert_eq!(
        row.get("caused_by_source_doc_id")
            .and_then(serde_json::Value::as_str),
        Some("source-doc-exact")
    );
    let request_doc_id = row["_docID"].as_str().unwrap();
    let queued =
        crate::request_admission::load_request_for_admission_test(node.as_ref(), request_doc_id)
            .await
            .unwrap();
    let (_authority_owner, authority) = crate::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        authority,
    );
    verifier.verify_fresh(&queued, "general").await.unwrap();
}

#[tokio::test]
async fn production_schedule_materialization_passes_final_exact_config_admission() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    let identity = behavior.principal_identity().clone();
    let owner = behavior.agent_did();
    crate::test_support::install_test_behavior(node.as_ref(), owner, "general").await;
    let config = serde_json::from_value(serde_json::json!({
        "agent_principal":{"agent_did":owner},
        "tasks":[{"agent_did":owner,"task_id":"task-schedule","behavior_id":"general","prompt_template":"schedule body"}],
        "schedules":[{"agent_did":owner,"schedule_id":"source","cadence":{"kind":"interval","interval_secs":60}}],
        "triggers":[{"agent_did":owner,"trigger_id":"schedule-trigger","task_id":"task-schedule","source":{"kind":"schedule","schedule_id":"source"}}]
    })).unwrap();
    let plan = crate::config_client::DesiredStateApplyPlan::from_pack_config(&config).unwrap();
    let trigger_doc_id = crate::config_client::ConfigAccess::transact_local(
        node.as_ref(),
        None,
        "test.manual_source.fixture",
        |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::config_client::apply_desired_state_plan(txn, plan).await?;
                let (id, _) = crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Trigger,
                    owner,
                    "schedule-trigger",
                )
                .await?
                .expect("created trigger");
                Ok(id)
            })
        },
    )
    .await
    .unwrap();
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), rx);
    let task = resolved_task_for_test("task-schedule", "general", "schedule body");
    let request_id = materializer
        .materialize(
            &task,
            Some("schedule-trigger"),
            TriggerKind::Schedule,
            Some(&trigger_doc_id),
            None,
            None,
            None,
            "schedule body",
            None,
            "schedule-test-fire",
        )
        .await
        .unwrap();
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID caused_by_trigger_doc_id caused_by_source_doc_id }} }}"#,
            escape_graphql_string(&request_id),
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load schedule request: {:?}",
        response.errors
    );
    let row = &response.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(row["caused_by_trigger_doc_id"], trigger_doc_id);
    assert!(row["caused_by_source_doc_id"].is_null());
    let queued = crate::request_admission::load_request_for_admission_test(
        node.as_ref(),
        row["_docID"].as_str().unwrap(),
    )
    .await
    .unwrap();
    let (_authority_owner, authority) = crate::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        authority,
    );
    verifier.verify_fresh(&queued, "general").await.unwrap();
}

#[tokio::test]
async fn production_materializer_rejects_incoherent_source_document_lineage() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    let snapshot =
        snapshot_with_behavior_and_schedules(integration_test_behavior("general"), HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node, rx);
    let task = resolved_task_for_test("task-lineage-input", "general", "body");

    let missing = materializer
        .materialize(
            &task,
            Some("event-trigger"),
            TriggerKind::Event,
            Some("event-trigger-config-doc"),
            None,
            None,
            None,
            "body",
            None,
            "invalid-event-test-fire",
        )
        .await
        .expect_err("Event materialization without a source document must fail closed");
    assert!(missing.to_string().contains("requires source_doc_id"));

    let misplaced = materializer
        .materialize(
            &task,
            Some("schedule-trigger"),
            TriggerKind::Schedule,
            Some("schedule-trigger-config-doc"),
            Some("event-doc-on-schedule"),
            None,
            None,
            "body",
            None,
            "invalid-schedule-test-fire",
        )
        .await
        .expect_err("non-Event materialization must reject source document lineage");
    assert!(misplaced.to_string().contains("Only Event"));
}

#[tokio::test]
async fn production_materializer_rejects_manual_lineage_with_trigger_id() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    let snapshot = snapshot_with_schedules(HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node, rx);
    let task = resolved_task_for_test("task-manual", "general", "manual body");

    let err = materializer
        .materialize(
            &task,
            Some("manual-must-not-have-id"),
            TriggerKind::Manual,
            None,
            None,
            None,
            None,
            "manual body",
            None,
            "invalid-manual-test-fire",
        )
        .await
        .expect_err("Manual materialize with trigger_id must fail before persistence");

    assert!(
        err.to_string().contains("must not carry trigger_id"),
        "unexpected manual lineage validation error: {err}"
    );
}

/// Task 6 pinning: `TriggerEngine::dispatch` must pass `TriggerKind::Manual`
/// intents through without consulting `active_schedules()` /
/// `active_event_triggers()` (no enabled-gate rejection for operator
/// fires), render the prompt template against `args_vars`, and invoke the
/// materializer exactly once with `(trigger_id = None, trigger_kind =
/// Manual, rendered = "hello Amy")`.
#[tokio::test]
async fn dispatch_manual_intent_renders_with_args_and_materializes() {
    // Snapshot carries the active task but NO active schedules / event
    // triggers. A Schedule/Event intent would be gated off here; Manual
    // must not be.
    let task = resolved_task_for_test("greet-user", "general", "hello {{ args.name }}");
    let snapshot = snapshot_with_active_task(task.clone());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let intent = FireIntent {
        trigger_id: None,
        trigger_kind: TriggerKind::Manual,
        task,
        concurrency: ConcurrencyMode::Parallel,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: Some(serde_json::json!({"name": "Amy"})),
        durable_fire_key: "manual-test-fire".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };

    let result = engine.dispatch(intent).await;

    match result {
        FireResult::Fired { request_id } => assert_eq!(
            request_id, "req-0",
            "spy materializer hands back sequentially-numbered ids starting at req-0"
        ),
        other => panic!("expected Fired for Manual intent (bypasses enabled-gate), got {other:?}"),
    }

    let calls = materializer.calls();
    assert_eq!(
        calls.len(),
        1,
        "exactly one materialize call expected for Manual dispatch"
    );
    let (trigger_id, kind, rendered) = &calls[0];
    assert!(
        trigger_id.is_none(),
        "Manual intents carry trigger_id = None; got {trigger_id:?}"
    );
    assert_eq!(*kind, TriggerKind::Manual);
    assert_eq!(
        rendered, "hello Amy",
        "dispatch must render the `args.name` template against args_vars"
    );
}

#[tokio::test]
async fn dispatch_rejects_manual_intent_with_trigger_id() {
    let task = resolved_task_for_test("greet-user", "general", "hello");
    let snapshot = snapshot_with_active_task(task.clone());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let result_captured: Arc<Mutex<Option<FireResult>>> = Arc::new(Mutex::new(None));
    let capture = result_captured.clone();
    let intent = FireIntent {
        trigger_id: Some("manual-must-not-have-id".to_string()),
        trigger_kind: TriggerKind::Manual,
        task,
        concurrency: ConcurrencyMode::Parallel,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: Some(serde_json::json!({})),
        durable_fire_key: "manual-invalid-fire".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(move |r| {
            *capture.lock().unwrap() = Some(r);
        }),
    };

    let result = engine.dispatch(intent).await;

    match result {
        FireResult::Errored { error } => assert!(
            error.contains("must not carry trigger_id"),
            "unexpected manual well-formedness error: {error}"
        ),
        other => panic!("expected Errored for malformed Manual intent, got {other:?}"),
    }
    assert!(
        materializer.calls().is_empty(),
        "malformed Manual intent must not reach the materializer"
    );
    assert!(
        matches!(
            result_captured.lock().unwrap().as_ref(),
            Some(FireResult::Errored { error }) if error.contains("must not carry trigger_id")
        ),
        "on_result should receive the same malformed Manual error"
    );
}
