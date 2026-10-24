use chrono::Timelike;

use super::*;

async fn read_trigger_cursor(
    node: &defra_node::EmbeddedNode,
    owner_did: &str,
    trigger_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut results = crate::document_config::load_trigger_next_run_at_batch(
        node,
        &[(owner_did.to_owned(), trigger_id.to_owned())],
    )
    .await;
    anyhow::ensure!(results.len() == 1, "singleton cursor result is missing");
    results.remove(0)
}

fn schedule_test_behavior() -> Arc<ResolvedAgent> {
    static BEHAVIOR: std::sync::OnceLock<Arc<ResolvedAgent>> = std::sync::OnceLock::new();
    BEHAVIOR
        .get_or_init(|| integration_test_agent("general"))
        .clone()
}

async fn create_schedule_cadence_doc(
    node: &Arc<defra_node::EmbeddedNode>,
    owner_did: &str,
    schedule_id: &str,
    cadence: serde_json::Value,
) {
    let input = serde_json::json!({
        "node_did": owner_did,
        "schedule_id": schedule_id,
        "cadence": cadence,
    });
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "test.schedule_cadence_fixture",
        |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input:ScheduleMutationInputArg!){create_Schedule(input:$input){_docID}}",
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

struct ScheduleFixture<'a> {
    trigger_id: &'a str,
    schedule_id: &'a str,
    task_id: &'a str,
    cadence: serde_json::Value,
    next_run_at: Option<&'a str>,
    concurrency: &'a str,
}

async fn create_schedule_with_next_run_at(
    node: &Arc<defra_node::EmbeddedNode>,
    owner_did: &str,
    fixture: ScheduleFixture<'_>,
) -> String {
    let ScheduleFixture {
        trigger_id,
        schedule_id,
        task_id,
        cadence,
        next_run_at,
        concurrency,
    } = fixture;
    create_schedule_cadence_doc(node, owner_did, schedule_id, cadence).await;
    let mut input = serde_json::json!({
        "node_did": owner_did,
        "trigger_id": trigger_id,
        "task_id": task_id,
        "source": {"kind": "schedule", "schedule_id": schedule_id},
        "enabled": true,
        "concurrency": concurrency,
        "fire_count": 0,
    });
    if let Some(next_run_at) = next_run_at {
        input["next_run_at"] = serde_json::Value::String(next_run_at.to_owned());
    }
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "test.schedule_trigger_fixture",
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

    let query = format!(
        r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
        escape_graphql_string(owner_did),
        escape_graphql_string(trigger_id),
    );
    let response = node.execute(&query).await;
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "Trigger")
        .expect("query seeded Trigger");
    assert_eq!(
        rows.len(),
        1,
        "seeded trigger {trigger_id} is ambiguous or missing"
    );
    rows[0]["_docID"]
        .as_str()
        .expect("Trigger._docID is a string")
        .to_string()
}

async fn observed_trigger(
    node: &defra_node::EmbeddedNode,
    owner_did: &str,
    trigger_id: &str,
) -> serde_json::Value {
    let query = format!(
        r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID task_id source enabled concurrency next_run_at last_attempt_at last_status last_error fire_count }} }}"#,
        escape_graphql_string(owner_did),
        escape_graphql_string(trigger_id),
    );
    let response = node.execute(&query).await;
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "Trigger")
        .expect("query Trigger observations");
    assert_eq!(
        rows.len(),
        1,
        "owner-scoped trigger {trigger_id} disappeared or is ambiguous"
    );
    rows[0].clone()
}

async fn observed_schedule_cadence(
    node: &defra_node::EmbeddedNode,
    owner_did: &str,
    schedule_id: &str,
) -> ScheduleCadence {
    let query = format!(
        r#"{{ Schedule(filter: {{ node_did: {{ _eq: "{}" }}, schedule_id: {{ _eq: "{}" }} }}, limit: 2) {{ cadence }} }}"#,
        escape_graphql_string(owner_did),
        escape_graphql_string(schedule_id),
    );
    let response = node.execute(&query).await;
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "Schedule")
        .expect("query Schedule cadence");
    assert_eq!(
        rows.len(),
        1,
        "owner-scoped schedule {schedule_id} disappeared or is ambiguous"
    );
    serde_json::from_value(rows[0]["cadence"].clone()).expect("decode canonical ScheduleCadence")
}

fn interval_cadence_json() -> serde_json::Value {
    serde_json::json!({"kind": "interval", "interval_secs": 60})
}

#[tokio::test]
async fn schedule_cursor_batch_matches_exact_owner_reads_and_isolates_scope_errors() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let owner_a = schedule_test_behavior().node_did().to_owned();
    let owner_b = "did:test:schedule-cursor-other-owner";
    let cursor_a = "2026-08-26T00:00:00Z";
    let cursor_b = "2026-08-27T00:00:00Z";
    create_schedule_with_next_run_at(
        &node,
        &owner_a,
        ScheduleFixture {
            trigger_id: "shared-trigger-id",
            schedule_id: "cursor-schedule-a",
            task_id: "task-a",
            cadence: interval_cadence_json(),
            next_run_at: Some(cursor_a),
            concurrency: "serial",
        },
    )
    .await;
    create_schedule_with_next_run_at(
        &node,
        owner_b,
        ScheduleFixture {
            trigger_id: "shared-trigger-id",
            schedule_id: "cursor-schedule-b",
            task_id: "task-b",
            cadence: interval_cadence_json(),
            next_run_at: Some(cursor_b),
            concurrency: "serial",
        },
    )
    .await;

    let scopes = vec![
        (owner_a.clone(), "shared-trigger-id".to_owned()),
        (owner_b.to_owned(), "shared-trigger-id".to_owned()),
        (String::new(), "shared-trigger-id".to_owned()),
    ];
    let batch =
        crate::document_config::load_trigger_next_run_at_batch(node.as_ref(), &scopes).await;
    assert_eq!(batch.len(), 3);
    assert_eq!(batch[0].as_ref().unwrap().as_deref(), Some(cursor_a));
    assert_eq!(batch[1].as_ref().unwrap().as_deref(), Some(cursor_b));
    assert!(
        batch[2].is_err(),
        "invalid owner error must remain per-trigger"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn schedule_cursor_batches_more_than_one_chunk() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let owner = schedule_test_behavior().node_did().to_owned();
    let scopes = (0..128)
        .map(|index| (owner.clone(), format!("missing-schedule-trigger-{index}")))
        .collect::<Vec<_>>();
    let (cursors, query_count) = crate::document_config::count_trigger_cursor_batch_queries(
        crate::document_config::load_trigger_next_run_at_batch(node.as_ref(), &scopes),
    )
    .await;

    assert_eq!(cursors.len(), 128);
    assert!(cursors.iter().all(|cursor| matches!(cursor, Ok(None))));
    assert_eq!(query_count, 2, "128 scopes use two 64-alias queries");
    node.shutdown().await;
}

#[tokio::test]
async fn schedule_source_stops_loading_chunks_after_first_due_trigger() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let task = resolved_task("due schedule");
    let schedules = (0..128)
        .map(|index| {
            let trigger_id = format!("lazy-schedule-trigger-{index}");
            (
                trigger_id.clone(),
                resolved_schedule(&trigger_id, task.clone()),
            )
        })
        .collect::<RapidHashMap<_, _>>();
    let snapshot = snapshot_with_schedules(schedules);
    let first_trigger_id = snapshot
        .active_schedules()
        .keys()
        .next()
        .expect("snapshot has active schedules")
        .clone();
    let owner = schedule_test_behavior().node_did().to_owned();
    let past = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
    create_schedule_with_next_run_at(
        &node,
        &owner,
        ScheduleFixture {
            trigger_id: &first_trigger_id,
            schedule_id: &first_trigger_id,
            task_id: &task.task_id,
            cadence: interval_cadence_json(),
            next_run_at: Some(&past),
            concurrency: "serial",
        },
    )
    .await;

    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(snapshot_rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(1));
    let (intent, query_count) =
        crate::document_config::count_trigger_cursor_batch_queries(source.next_fire()).await;

    let intent = intent.expect("first active schedule is due");
    assert_eq!(
        intent.trigger_id.as_deref(),
        Some(first_trigger_id.as_str())
    );
    assert_eq!(
        query_count, 1,
        "later cursor chunks are not read after a due trigger"
    );
    drop(intent);
    cancel.cancel();
    node.shutdown().await;
}

#[tokio::test]
async fn schedule_source_next_fire_emits_intent_when_schedule_is_due() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let behavior = schedule_test_behavior();
    let owner_did = behavior.node_did().to_string();
    let past = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
    let trigger_doc_id = create_schedule_with_next_run_at(
        &node,
        &owner_did,
        ScheduleFixture {
            trigger_id: "sched-1",
            schedule_id: "sched-1",
            task_id: "task-1",
            cadence: interval_cadence_json(),
            next_run_at: Some(&past),
            concurrency: "serial",
        },
    )
    .await;
    let persisted_next_run_at = read_trigger_cursor(node.as_ref(), &owner_did, "sched-1")
        .await
        .unwrap()
        .unwrap();

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-1".to_string(),
        name: None,
        agent_id: "general".to_string(),
        prompt_template: "hi".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let schedule = ResolvedSchedule {
        session_id_template: None,
        trigger_doc_id,
        schedule_id: "sched-1".to_string(),
        task_id: task.task_id.clone(),
        task,
        cadence: ScheduleCadence::Interval { interval_secs: 60 },
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    };
    let snapshot = snapshot_with_agent_and_schedules(
        behavior,
        RapidHashMap::from_iter([("sched-1".to_string(), schedule)]),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out")
        .expect("next_fire returned None");

    assert_eq!(intent.trigger_id.as_deref(), Some("sched-1"));
    assert_eq!(intent.trigger_kind, TriggerKind::Schedule);
    assert_eq!(intent.concurrency, ConcurrencyMode::Serial);
    assert_eq!(intent.task.task_id, "task-1");
    assert!(intent.doc_vars.is_none());
    assert!(intent.args_vars.is_none());
    assert_eq!(
        intent.durable_fire_key,
        crate::trigger_engine::durable_fire_key("schedule", &["sched-1", &persisted_next_run_at])
    );

    let ev = &intent.event_vars;
    assert_eq!(ev["trigger_id"].as_str(), Some("sched-1"));
    assert_eq!(ev["trigger_kind"].as_str(), Some("schedule"));
    assert!(
        ev["fired_at"].is_string(),
        "fired_at should be a string, got {:?}",
        ev["fired_at"]
    );
    let retry = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("unacknowledged occurrence remains due")
        .expect("schedule source remains active");
    assert_eq!(retry.durable_fire_key, intent.durable_fire_key);
}

#[tokio::test]
async fn schedule_source_on_result_writes_runtime_fields_on_fired_and_skipped() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let behavior = schedule_test_behavior();
    let owner_did = behavior.node_did().to_string();

    let initial_next_run_at = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
    let trigger_doc_id = create_schedule_with_next_run_at(
        &node,
        &owner_did,
        ScheduleFixture {
            trigger_id: "sched-1",
            schedule_id: "sched-1",
            task_id: "task-1",
            cadence: interval_cadence_json(),
            next_run_at: Some(&initial_next_run_at),
            concurrency: "serial",
        },
    )
    .await;

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-1".to_string(),
        name: None,
        agent_id: "general".to_string(),
        prompt_template: "hi".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let schedule = ResolvedSchedule {
        session_id_template: None,
        trigger_doc_id,
        schedule_id: "sched-1".to_string(),
        task_id: task.task_id.clone(),
        task,
        cadence: ScheduleCadence::Interval { interval_secs: 60 },
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    };
    let interval_secs = match schedule.cadence {
        ScheduleCadence::Interval { interval_secs } => interval_secs,
        ScheduleCadence::Cron { .. } => panic!("test helper should build an interval schedule"),
    };
    let snapshot = snapshot_with_agent_and_schedules(
        behavior,
        RapidHashMap::from_iter([("sched-1".to_string(), schedule)]),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));

    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out (fired)")
        .expect("next_fire returned None (fired)");
    (intent.on_result)(FireResult::Fired {
        request_id: "req-0".to_string(),
    });
    let expected_next_run_at_fired = (DateTime::parse_from_rfc3339(&initial_next_run_at)
        .unwrap()
        .with_timezone(&Utc)
        + ChronoDuration::seconds(interval_secs))
    .to_rfc3339();
    let mut fired_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let trigger = observed_trigger(node.as_ref(), &owner_did, "sched-1").await;
        if trigger["last_status"].as_str() == Some("fired") {
            fired_trigger = Some(trigger);
            break;
        }
    }
    let fired = fired_trigger.expect("Trigger.last_status never became \"fired\"");
    assert_eq!(fired["last_status"].as_str(), Some("fired"));
    assert_eq!(fired["fire_count"].as_i64(), Some(1));
    let fired_next = fired["next_run_at"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc).timestamp());
    let expected_next_fired = DateTime::parse_from_rfc3339(&expected_next_run_at_fired)
        .unwrap()
        .with_timezone(&Utc)
        .timestamp();
    assert_eq!(fired_next, Some(expected_next_fired));
    assert!(
        fired["last_attempt_at"].is_string(),
        "last_attempt_at should be set after a fire: {}",
        fired
    );
    assert_eq!(fired["task_id"].as_str(), Some("task-1"));
    assert_eq!(fired["enabled"].as_bool(), Some(true));
    assert_eq!(fired["concurrency"].as_str(), Some("serial"));
    assert_eq!(
        fired["source"]["kind"].as_str(),
        Some("schedule"),
        "Trigger.source must keep pointing at the Schedule: {fired}"
    );
    assert_eq!(
        observed_schedule_cadence(node.as_ref(), &owner_did, "sched-1").await,
        ScheduleCadence::Interval { interval_secs: 60 },
    );

    let rewound_next_run_at =
        (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let escaped_owner = escape_graphql_string(&owner_did);
    let escaped_trigger_id = escape_graphql_string("sched-1");
    let escaped_rewound = escape_graphql_string(&rewound_next_run_at);
    let preserved_last_attempt = fired["last_attempt_at"]
        .as_str()
        .expect("last_attempt_at must be set after the fired writeback")
        .to_string();
    let escaped_preserved_last_attempt = escape_graphql_string(&preserved_last_attempt);
    let mutation = format!(
        r#"mutation {{
            update_Trigger(
                filter: {{ node_did: {{ _eq: "{escaped_owner}" }}, trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                input: {{
                    next_run_at: "{escaped_rewound}",
                    last_attempt_at: "{escaped_preserved_last_attempt}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "rewind mutation failed: {:?}",
        resp.errors
    );

    let intent2 = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire timed out (skipped)")
        .expect("next_fire returned None (skipped)");
    (intent2.on_result)(FireResult::Skipped {
        reason: "serial: prior fire still in-flight".to_string(),
    });
    let expected_next_run_at_skipped = (DateTime::parse_from_rfc3339(&rewound_next_run_at)
        .unwrap()
        .with_timezone(&Utc)
        + ChronoDuration::seconds(interval_secs))
    .to_rfc3339();
    let mut skipped_trigger = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let trigger = observed_trigger(node.as_ref(), &owner_did, "sched-1").await;
        if trigger["last_status"].as_str() == Some("skipped") {
            skipped_trigger = Some(trigger);
            break;
        }
    }
    let skipped = skipped_trigger.expect("Trigger.last_status never became \"skipped\"");
    assert_eq!(skipped["last_status"].as_str(), Some("skipped"));
    assert_eq!(skipped["fire_count"].as_i64(), Some(1));
    let skipped_next = skipped["next_run_at"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc).timestamp());
    let expected_next_skipped = DateTime::parse_from_rfc3339(&expected_next_run_at_skipped)
        .unwrap()
        .with_timezone(&Utc)
        .timestamp();
    assert_eq!(skipped_next, Some(expected_next_skipped));
    assert_eq!(skipped["task_id"].as_str(), Some("task-1"));
    assert_eq!(skipped["enabled"].as_bool(), Some(true));
    assert_eq!(skipped["concurrency"].as_str(), Some("serial"));
    assert_eq!(
        observed_schedule_cadence(node.as_ref(), &owner_did, "sched-1").await,
        ScheduleCadence::Interval { interval_secs: 60 },
    );
}

#[tokio::test]
async fn schedule_source_next_fire_honors_cancellation_token() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let snapshot = snapshot_with_schedules(RapidHashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_secs(30));

    cancel.cancel();

    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("next_fire did not return within 2s after cancel");
    let elapsed = start.elapsed();

    assert!(
        result.is_none(),
        "expected None after cancel, got Some(intent) with trigger_id={:?}",
        result.as_ref().and_then(|i| i.trigger_id.as_deref())
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "next_fire should return promptly on cancel, took {elapsed:?}"
    );
}

#[tokio::test]
async fn trigger_engine_enqueues_agent_request_for_due_schedule_e2e() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let behavior = schedule_test_behavior();
    let owner_did = behavior.node_did().to_string();
    let past = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
    let trigger_doc_id = create_schedule_with_next_run_at(
        &node,
        &owner_did,
        ScheduleFixture {
            trigger_id: "sched-e2e",
            schedule_id: "sched-e2e",
            task_id: "task-e2e",
            cadence: interval_cadence_json(),
            next_run_at: Some(&past),
            concurrency: "serial",
        },
    )
    .await;

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-e2e".to_string(),
        name: Some("Mini Host Health".to_string()),
        agent_id: behavior.agent_id.clone(),
        prompt_template: "integration fire".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let schedule = ResolvedSchedule {
        session_id_template: None,
        trigger_doc_id,
        schedule_id: "sched-e2e".to_string(),
        task_id: task.task_id.clone(),
        task,
        cadence: ScheduleCadence::Interval { interval_secs: 60 },
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    };
    let snapshot = snapshot_with_agent_and_schedules(
        behavior,
        RapidHashMap::from_iter([("sched-e2e".to_string(), schedule)]),
    );
    let (_tx, rx) = watch::channel(snapshot);

    let cancel = CancellationToken::new();
    let materializer: Arc<dyn MaterializerHandle> =
        Arc::new(ProductionMaterializer::new(node.clone(), rx.clone()));
    let source: Box<dyn TriggerSource> = Box::new(
        ScheduleSource::new(rx.clone(), node.clone(), cancel.clone())
            .with_tick_every(Duration::from_millis(50)),
    );
    let engine = TriggerEngine::new(rx, materializer);
    let engine_cancel = cancel.clone();
    let engine_handle = tokio::spawn(async move {
        engine.run(vec![source], engine_cancel).await;
    });

    let mut observed = None;
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let query = r#"query {
            AgentRequest(filter: {
                caused_by_trigger_id: { _eq: "sched-e2e" },
                caused_by_trigger_kind: { _eq: "schedule" }
            }) {
                _docID
                caused_by_trigger_id
                caused_by_trigger_kind
                lifecycle_state
                execution_origin
                session_id
                content
                input
            }
        }"#;
        let resp = node.execute(query).await;
        assert!(
            !resp.has_errors(),
            "AgentRequest query errored: {:?}",
            resp.errors
        );
        let rows = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !rows.is_empty() {
            observed = rows.into_iter().next();
            break;
        }
    }

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), engine_handle).await;

    let row = observed.expect(
        "no AgentRequest with caused_by_trigger_id=sched-e2e observed within 4s; \
         expected the TriggerEngine + ScheduleSource pipeline to have materialized one",
    );
    assert_eq!(
        row.get("caused_by_trigger_id").and_then(|v| v.as_str()),
        Some("sched-e2e"),
        "persisted request is missing caused_by_trigger_id lineage: {row}"
    );
    assert_eq!(
        row.get("caused_by_trigger_kind").and_then(|v| v.as_str()),
        Some("schedule"),
        "persisted request is missing caused_by_trigger_kind lineage: {row}"
    );
    assert_eq!(
        row.get("execution_origin").and_then(|v| v.as_str()),
        Some("scheduled"),
        "trigger-driven fire should set execution_origin=scheduled: {row}"
    );
    assert_eq!(
        row.get("lifecycle_state").and_then(|v| v.as_str()),
        Some("pending"),
        "ProductionMaterializer should enqueue pending requests for watcher/router execution: {row}"
    );
    let content = row
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        content.starts_with("[scheduled trigger:sched-e2e · task:task-e2e · at:20")
            && content.ends_with("]\n\nintegration fire"),
        "rendered prompt template should land in AgentRequest.content: {row}"
    );

    let input = row
        .get("input")
        .and_then(|value| value.as_object().cloned())
        .expect("task request should carry typed RequestInput");
    let title = input
        .get("initial_title")
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .expect("task request should carry an initial_title");
    assert_eq!(
        input
            .get("initial_title")
            .and_then(|v| v.get("source"))
            .and_then(|v| v.as_str()),
        Some("task"),
        "task-fired request title must carry the task source: {input:?}"
    );
    assert!(
        title.starts_with("mini-host-health-20"),
        "task request title should use task name plus timestamp: {input:?}"
    );
}

#[tokio::test]
async fn schedule_source_seeds_null_next_run_at_and_fires_on_first_tick() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = schedule_test_behavior();
    let owner_did = behavior.node_did().to_string();
    let trigger_doc_id = create_schedule_with_next_run_at(
        &node,
        &owner_did,
        ScheduleFixture {
            trigger_id: "sched-null",
            schedule_id: "sched-null",
            task_id: "task-null",
            cadence: interval_cadence_json(),
            next_run_at: None,
            concurrency: "serial",
        },
    )
    .await;
    let precondition = read_trigger_cursor(node.as_ref(), &owner_did, "sched-null")
        .await
        .unwrap();
    assert!(
        precondition.is_none(),
        "precondition: created Trigger should have null next_run_at, got {precondition:?}"
    );
    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-null".to_string(),
        name: None,
        agent_id: "general".to_string(),
        prompt_template: "hi".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let schedule = ResolvedSchedule {
        session_id_template: None,
        trigger_doc_id,
        schedule_id: "sched-null".to_string(),
        task_id: task.task_id.clone(),
        task,
        cadence: ScheduleCadence::Interval { interval_secs: 60 },
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    };
    let snapshot = snapshot_with_agent_and_schedules(
        behavior,
        RapidHashMap::from_iter([("sched-null".to_string(), schedule)]),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));
    let started = Instant::now();
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect(
            "next_fire did not yield a FireIntent within 2s for a trigger with null \
             next_run_at; the engine must seed the Trigger cursor on first-seen (Finding 2)",
        )
        .expect("next_fire returned None for a trigger with null next_run_at");
    let elapsed = started.elapsed();
    assert_eq!(intent.trigger_id.as_deref(), Some("sched-null"));
    assert_eq!(intent.trigger_kind, TriggerKind::Schedule);
    assert!(
        elapsed < Duration::from_secs(2),
        "first-tick fire should land within a couple of ticks, took {elapsed:?}"
    );
    let after_seed = read_trigger_cursor(node.as_ref(), &owner_did, "sched-null")
        .await
        .unwrap();
    assert!(
        after_seed.is_some(),
        "Trigger.next_run_at should no longer be null after first-seen seeding"
    );
}

#[tokio::test]
async fn schedule_source_seeds_cron_next_run_at_without_immediate_fire() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let behavior = schedule_test_behavior();
    let owner_did = behavior.node_did().to_string();

    let trigger_doc_id = create_schedule_with_next_run_at(
        &node,
        &owner_did,
        ScheduleFixture {
            trigger_id: "sched-cron-null",
            schedule_id: "sched-cron-null",
            task_id: "task-cron",
            cadence: serde_json::json!({
                "kind": "cron",
                "expression": "30 0 * * *",
                "timezone": "America/Los_Angeles",
                "missed_run_policy": "latest_only"
            }),
            next_run_at: None,
            concurrency: "serial",
        },
    )
    .await;

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-cron".to_string(),
        name: None,
        agent_id: "general".to_string(),
        prompt_template: "hi".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let schedule = ResolvedSchedule {
        session_id_template: None,
        trigger_doc_id,
        schedule_id: "sched-cron-null".to_string(),
        task_id: task.task_id.clone(),
        task,
        cadence: ScheduleCadence::Cron {
            expression: "30 0 * * *".to_string(),
            timezone: "America/Los_Angeles".to_string(),
            missed_run_policy: Some(crate::schedule_cron::CronMissedRunPolicy::LatestOnly),
        },
        enabled: true,
        concurrency: ConcurrencyMode::Serial,
    };
    let snapshot = snapshot_with_agent_and_schedules(
        behavior,
        RapidHashMap::from_iter([("sched-cron-null".to_string(), schedule)]),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));
    let started_at = Utc::now();

    let handle = tokio::spawn(async move { source.next_fire().await });
    let seeded = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match read_trigger_cursor(node.as_ref(), &owner_did, "sched-cron-null")
                .await
                .unwrap()
            {
                Some(value) => break value,
                None => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
    })
    .await
    .expect("cron schedule should seed the Trigger cursor within 2s");

    cancel.cancel();
    assert!(
        handle.await.unwrap().is_none(),
        "cron schedule should stay idle after seeding a future cursor"
    );

    let parsed = DateTime::parse_from_rfc3339(&seeded)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        parsed > started_at,
        "cron first seed should be in the future, got {seeded}"
    );
    let timezone = crate::schedule_cron::parse_timezone("America/Los_Angeles").unwrap();
    let local = parsed.with_timezone(&timezone);
    assert_eq!(local.hour(), 0);
    assert_eq!(local.minute(), 30);
    let started_local_date = started_at.with_timezone(&timezone).date_naive();
    let next_local_date = started_local_date
        .succ_opt()
        .expect("test date should have a next day");
    assert!(
        local.date_naive() == started_local_date || local.date_naive() == next_local_date,
        "daily cron should seed to today or tomorrow in local time, got {local}"
    );
}

#[tokio::test]
async fn schedule_source_next_fire_survives_idle_ticks_until_cancelled() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let snapshot = snapshot_with_schedules(RapidHashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let cancel = CancellationToken::new();
    let mut source = ScheduleSource::new(rx, node.clone(), cancel.clone())
        .with_tick_every(Duration::from_millis(50));

    let cancel_clone = cancel.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        cancel_clone.cancel();
    });

    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(3), source.next_fire())
        .await
        .expect("next_fire did not return within 3s after cancel");
    let elapsed_until_return = started.elapsed();
    let _ = canceller.await;

    assert!(
        result.is_none(),
        "idle source should only return None after cancel, got Some(intent)",
    );
    assert!(
        elapsed_until_return >= Duration::from_millis(240),
        "next_fire returned before cancel fired at ~250ms (elapsed={elapsed_until_return:?}); \
         this means the source treated an idle tick as exhaustion and returned None early"
    );
}
