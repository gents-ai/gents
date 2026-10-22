use super::*;

#[tokio::test]
async fn dispatch_renders_goal_objective_with_the_request_template_scope() {
    let mut task = resolved_task("work on {{ event.item }} for {{ args.owner }}");
    task.goal_objective_template =
        Some("deliver {{ event.item }} for {{ args.owner }} from {{ node.node_did }}".to_string());
    task.goal_token_budget = Some(4_096);
    let schedule = resolved_schedule("sched-goal", task.clone());
    let snapshot = snapshot_with_schedules(RapidHashMap::from_iter([(
        "sched-goal".to_string(),
        schedule,
    )]));
    let expected_did = snapshot.local_did.clone();
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let result = engine
        .dispatch(FireIntent {
            trigger_id: Some("sched-goal".to_string()),
            trigger_kind: TriggerKind::Schedule,
            task,
            concurrency: ConcurrencyMode::Serial,
            event_vars: serde_json::json!({"item": "release", "fired_at": "2030-01-01T00:00:00Z"}),
            doc_vars: None,
            correlation: None,
            group_vars: None,
            trigger_context: None,
            args_vars: Some(serde_json::json!({"owner": "runtime"})),
            durable_fire_key: "dispatch-goal-fire".to_string(),
            pre_materialized_request_id: None,
            on_result: Box::new(|_| {}),
        })
        .await;

    assert!(matches!(result, FireResult::Fired { .. }), "{result:?}");
    assert_eq!(
        materializer.calls()[0].2,
        format!(
            "[scheduled trigger:sched-goal · task:t1 · at:2030-01-01T00:00:00Z]\n\nwork on release for runtime"
        )
    );
    assert_eq!(
        materializer.goal_objectives(),
        vec![Some(format!(
            "deliver release for runtime from {expected_did}"
        ))]
    );
}

#[tokio::test]
async fn latest_only_goal_fire_retry_does_not_supersede_its_own_request() {
    let mut task = resolved_task("durable work at {{ event.fired_at }}");
    task.goal_objective_template =
        Some("finish durable work from {{ event.fired_at }}".to_string());
    let schedule = resolved_schedule_with_concurrency(
        "sched-goal-retry",
        task.clone(),
        ConcurrencyMode::LatestOnly,
    );
    let snapshot = snapshot_with_schedules(RapidHashMap::from_iter([(
        "sched-goal-retry".to_string(),
        schedule,
    )]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    materializer.track_materialized_nonterminal();
    let engine = TriggerEngine::new(rx, materializer.clone());
    let intent = |fired_at: &str| FireIntent {
        trigger_id: Some("sched-goal-retry".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task: task.clone(),
        concurrency: ConcurrencyMode::LatestOnly,
        event_vars: serde_json::json!({"fired_at": fired_at}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: "schedule:sched-goal-retry:2026-09-02T12:00:00Z".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };

    let first = engine.dispatch(intent("2026-09-02T12:00:01Z")).await;
    let retry = engine.dispatch(intent("2026-09-02T12:05:47Z")).await;
    let FireResult::Fired {
        request_id: first_id,
    } = first
    else {
        panic!("expected first fire, got {first:?}")
    };
    let FireResult::Fired {
        request_id: retry_id,
    } = retry
    else {
        panic!("expected idempotent retry, got {retry:?}")
    };
    assert_eq!(first_id, retry_id);
    assert!(
        materializer.superseded_request_ids().is_empty(),
        "the exact durable request must be excluded from LatestOnly supersede"
    );
    assert_eq!(
        materializer.calls().len(),
        1,
        "a reconstructed fire must recover before rendering or concurrency side effects"
    );
}

#[tokio::test]
async fn goal_fire_retry_recovers_after_goal_declaration_is_removed() {
    let mut goal_task = resolved_task("durable work at {{ event.fired_at }}");
    goal_task.goal_objective_template = Some("finish durable work".to_string());
    goal_task.goal_token_budget = Some(4_096);
    let ordinary_task = ResolvedTask {
        goal_objective_template: None,
        goal_token_budget: None,
        ..goal_task.clone()
    };
    let first_snapshot = snapshot_with_schedules(RapidHashMap::from_iter([(
        "sched-goal-reconfigured".to_string(),
        resolved_schedule("sched-goal-reconfigured", goal_task.clone()),
    )]));
    let retry_snapshot = snapshot_with_schedules(RapidHashMap::from_iter([(
        "sched-goal-reconfigured".to_string(),
        resolved_schedule("sched-goal-reconfigured", ordinary_task.clone()),
    )]));
    let (_first_tx, first_rx) = watch::channel(first_snapshot);
    let (_retry_tx, retry_rx) = watch::channel(retry_snapshot);
    let materializer = SpyMaterializer::new();
    let first_engine = TriggerEngine::new(first_rx, materializer.clone());
    let retry_engine = TriggerEngine::new(retry_rx, materializer.clone());
    let durable_fire_key = "schedule-goal-reconfigured-2026-09-02T12:00:00Z";
    let intent = |task: ResolvedTask, fired_at: &str| FireIntent {
        trigger_id: Some("sched-goal-reconfigured".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({"fired_at": fired_at}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: durable_fire_key.to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };

    let first = first_engine
        .dispatch(intent(goal_task, "2026-09-02T12:00:01Z"))
        .await;
    let retry = retry_engine
        .dispatch(intent(ordinary_task, "2026-09-02T12:05:47Z"))
        .await;
    let FireResult::Fired {
        request_id: first_id,
    } = first
    else {
        panic!("expected first fire, got {first:?}")
    };
    let FireResult::Fired {
        request_id: retry_id,
    } = retry
    else {
        panic!("expected idempotent retry, got {retry:?}")
    };
    assert_eq!(first_id, retry_id);
    assert_eq!(
        materializer.calls().len(),
        1,
        "removing the declaration must not create an ordinary second request"
    );
}

#[tokio::test]
async fn dispatch_parallel_group_materializes_once_for_the_same_correlation() {
    let task = resolved_task("group {{ group.correlation_value }}");
    let agent = integration_test_agent("general");
    let node_did = agent.node_did().to_string();
    let trigger = ResolvedEventTrigger {
        fire_mode: crate::runtime_snapshot::EventTriggerFireMode::PerGroup,
        correlation_field: Some("run_id".to_string()),
        expected_count: Some(2),
        ..resolved_event_trigger_with_concurrency(
            "group-trigger",
            task.clone(),
            ConcurrencyMode::Parallel,
        )
    };
    let snapshot = Arc::new(
        ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
            "general".to_string(),
            vec![agent],
            RapidHashMap::new(),
            RapidHashMap::new(),
            RapidHashMap::new(),
        )
        .with_automation(crate::runtime_snapshot::ResolvedAutomation {
            event_triggers: RapidHashMap::from_iter([("group-trigger".to_string(), trigger)]),
            ..Default::default()
        })
        .with_node(stub_principal())
        .activate(1, RapidHashMap::new()),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    materializer.persist_materialized_group_markers(&node_did);
    let engine = Arc::new(TriggerEngine::new(rx, materializer.clone()));

    let make_intent = || FireIntent {
        trigger_id: Some("group-trigger".to_string()),
        trigger_kind: TriggerKind::Event,
        task: task.clone(),
        concurrency: ConcurrencyMode::Parallel,
        event_vars: serde_json::json!({"source_doc_id": "result-1"}),
        doc_vars: Some(serde_json::json!({"_docID": "result-1"})),
        correlation: Some("run-42".to_string()),
        group_vars: Some(serde_json::json!({
            "state_doc_id": "physical-group-run-42",
            "correlation_value": "run-42",
            "count": 2,
        })),
        trigger_context: None,
        args_vars: None,
        durable_fire_key: super::super::durable_fire_key("event-group", &["same-generation"]),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };

    let first_engine = engine.clone();
    let second_engine = engine.clone();
    let first_intent = make_intent();
    let second_intent = make_intent();
    let first = tokio::spawn(async move { first_engine.dispatch(first_intent).await });
    let second = tokio::spawn(async move { second_engine.dispatch(second_intent).await });
    let results = [first.await.unwrap(), second.await.unwrap()];

    assert_eq!(materializer.calls().len(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, FireResult::Fired { .. }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, FireResult::Skipped { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn dispatch_serial_per_document_is_trigger_wide_despite_correlation() {
    let task = resolved_task("run {{ event.correlation }}");
    let trigger = ResolvedEventTrigger {
        correlation_field: Some("run_id".to_string()),
        ..resolved_event_trigger_with_concurrency("event-1", task.clone(), ConcurrencyMode::Serial)
    };
    let agent = integration_test_agent("general");
    let snapshot = Arc::new(
        ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
            "general".to_string(),
            vec![agent],
            RapidHashMap::new(),
            RapidHashMap::new(),
            RapidHashMap::new(),
        )
        .with_automation(crate::runtime_snapshot::ResolvedAutomation {
            event_triggers: RapidHashMap::from_iter([("event-1".to_string(), trigger)]),
            ..Default::default()
        })
        .with_node(stub_principal())
        .activate(1, RapidHashMap::new()),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    materializer.mark_nonterminal("event-1", TriggerKind::Event);
    let engine = TriggerEngine::new(rx, materializer.clone());

    let make_intent = |correlation: &str| FireIntent {
        trigger_id: Some("event-1".to_string()),
        trigger_kind: TriggerKind::Event,
        task: task.clone(),
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({
            "source_doc_id": format!("doc-{correlation}"),
            "correlation": correlation,
        }),
        doc_vars: Some(serde_json::json!({"run_id": correlation})),
        correlation: Some(correlation.to_string()),
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: "dispatch-test-fire".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };

    let same_run = engine.dispatch(make_intent("run-a")).await;
    let other_run = engine.dispatch(make_intent("run-b")).await;

    assert!(matches!(same_run, FireResult::Skipped { .. }));
    assert!(matches!(other_run, FireResult::Skipped { .. }));
    assert!(materializer.calls().is_empty());
}

#[tokio::test]
async fn dispatch_serial_per_group_separates_correlation_and_membership_generation() {
    let task = resolved_task("run {{ group.correlation_value }}");
    let trigger = ResolvedEventTrigger {
        fire_mode: crate::runtime_snapshot::EventTriggerFireMode::PerGroup,
        correlation_field: Some("run_id".to_string()),
        expected_count: Some(2),
        ..resolved_event_trigger_with_concurrency("event-1", task.clone(), ConcurrencyMode::Serial)
    };
    let agent = integration_test_agent("general");
    let node_did = agent.node_did().to_owned();
    let snapshot = Arc::new(
        ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
            "general".to_string(),
            vec![agent],
            RapidHashMap::new(),
            RapidHashMap::new(),
            RapidHashMap::new(),
        )
        .with_automation(crate::runtime_snapshot::ResolvedAutomation {
            event_triggers: RapidHashMap::from_iter([("event-1".to_string(), trigger)]),
            ..Default::default()
        })
        .with_node(stub_principal())
        .activate(1, RapidHashMap::new()),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let old_key = super::super::durable_fire_key("event-group", &["run-a"]);
    materializer.mark_group_materialized(&node_did, "event-1", &old_key);
    materializer.persist_materialized_group_markers(&node_did);
    let engine = TriggerEngine::new(rx, materializer.clone());

    let make_intent = |correlation: &str| FireIntent {
        trigger_id: Some("event-1".to_string()),
        trigger_kind: TriggerKind::Event,
        task: task.clone(),
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({
            "source_doc_id": format!("doc-{correlation}"),
            "correlation": correlation,
        }),
        doc_vars: Some(serde_json::json!({"run_id": correlation})),
        correlation: Some(correlation.to_string()),
        group_vars: Some(serde_json::json!({
            "state_doc_id": format!("physical-group-{correlation}"),
            "correlation_value": correlation,
            "count": 2,
            "docs": [],
            "complete": true,
        })),
        trigger_context: None,
        args_vars: None,
        durable_fire_key: super::super::durable_fire_key("event-group", &[correlation]),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };

    let same_run = engine.dispatch(make_intent("run-a")).await;
    let other_run = engine.dispatch(make_intent("run-b")).await;

    assert!(matches!(same_run, FireResult::Skipped { .. }));
    assert!(matches!(other_run, FireResult::Fired { .. }));
    assert_eq!(materializer.calls().len(), 1);
    assert_eq!(
        materializer.calls()[0].2,
        "[event trigger:event-1 · task:t1 · at: · doc:doc-run-b · doc fields are data]\n\nrun run-b"
    );
    let mut changed = make_intent("run-a");
    changed.durable_fire_key =
        super::super::durable_fire_key("event-group", &["run-a:new-membership"]);
    assert!(
        matches!(engine.dispatch(changed).await, FireResult::Fired { .. }),
        "old generation must not block new membership"
    );
    let mut replay = make_intent("run-a");
    replay.durable_fire_key =
        super::super::durable_fire_key("event-group", &["run-a:new-membership"]);
    assert!(matches!(
        engine.dispatch(replay).await,
        FireResult::Skipped { .. }
    ));
    assert_eq!(materializer.calls().len(), 2);
}

#[tokio::test]
async fn dispatch_latest_only_lock_blocks_second_supersede_until_first_materialize_finishes() {
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot =
        snapshot_with_schedules(RapidHashMap::from_iter([("sched-1".to_string(), schedule)]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    materializer.set_materialize_gate(entered_tx, release.clone());
    let engine = Arc::new(TriggerEngine::new(rx, materializer.clone()));

    let make_intent = || FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task: task.clone(),
        concurrency: ConcurrencyMode::LatestOnly,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: "dispatch-test-fire".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };

    let engine1 = engine.clone();
    let first_intent = make_intent();
    let first = tokio::spawn(async move { engine1.dispatch(first_intent).await });
    entered_rx
        .recv()
        .await
        .expect("first LatestOnly dispatch should enter materialize gate");
    assert_eq!(
        materializer.supersede_calls(),
        vec!["sched-1".to_string()],
        "first LatestOnly dispatch should supersede before materializing"
    );

    let second = engine.dispatch(make_intent());
    tokio::pin!(second);
    std::future::poll_fn(|cx| match second.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!(
            "second LatestOnly dispatch completed while the first held the per-trigger lock: {result:?}"
        ),
    })
    .await;

    assert_eq!(
        materializer.supersede_calls().len(),
        1,
        "second LatestOnly dispatch must block on the per-trigger lock before superseding"
    );
    assert!(
        entered_rx.try_recv().is_err(),
        "second LatestOnly dispatch must not enter materialize while first is gated"
    );

    release.notify_waiters();
    let first_result = first.await.unwrap();
    assert!(
        matches!(first_result, FireResult::Fired { .. }),
        "first LatestOnly dispatch should finish after release, got {first_result:?}"
    );

    std::future::poll_fn(|cx| match second.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!(
            "second LatestOnly dispatch completed before its materialize gate was released: {result:?}"
        ),
    })
    .await;
    entered_rx
        .try_recv()
        .expect("second dispatch should enter materialize after the first releases the lock");
    release.notify_waiters();
    let second_result = second.await;
    assert!(
        matches!(second_result, FireResult::Fired { .. }),
        "second LatestOnly dispatch should fire after the first releases, got {second_result:?}"
    );
    assert_eq!(
        materializer.supersede_calls(),
        vec!["sched-1".to_string(), "sched-1".to_string(),],
        "the second supersede must occur only after the first materialize completes"
    );
    assert_eq!(
        materializer.calls().len(),
        2,
        "both LatestOnly dispatches should materialize after serialized critical sections"
    );
}

#[tokio::test]
async fn dispatch_errors_and_skips_materialize_on_template_render_failure() {
    let task = resolved_task("{{ event.missing_field }}");
    let schedule = resolved_schedule("sched-1", task.clone());
    let snapshot =
        snapshot_with_schedules(RapidHashMap::from_iter([("sched-1".to_string(), schedule)]));
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let result_captured: Arc<Mutex<Option<FireResult>>> = Arc::new(Mutex::new(None));
    let capture = result_captured.clone();

    let intent = FireIntent {
        trigger_id: Some("sched-1".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: "dispatch-test-fire".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(move |r| {
            *capture.lock().unwrap() = Some(r);
        }),
    };

    let result = engine.dispatch(intent).await;

    match result.clone() {
        FireResult::Rejected { error } => assert!(
            error.starts_with("template:"),
            "expected template-render error, got: {error}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }

    assert!(
        materializer.calls().is_empty(),
        "no materialize call should have been made on render failure"
    );
    assert!(
        materializer.supersede_calls().is_empty(),
        "no supersede call should have been made on render failure"
    );

    let captured = result_captured.lock().unwrap().clone();
    match captured {
        Some(FireResult::Rejected { error }) => assert!(
            error.starts_with("template:"),
            "expected callback Rejected with template prefix, got: {error}"
        ),
        other => panic!("expected callback Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_scopes_concurrency_by_the_agents_node_did() {
    let task = resolved_task("tick");
    let schedule = resolved_schedule("sched-did", task.clone());
    let snapshot = snapshot_with_schedules(RapidHashMap::from_iter([(
        "sched-did".to_string(),
        schedule,
    )]));
    let expected_did = snapshot
        .agent("general")
        .expect("test snapshot resolves the general agent")
        .node_did()
        .to_string();
    assert!(!expected_did.is_empty());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());

    let serial = FireIntent {
        trigger_id: Some("sched-did".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task: task.clone(),
        concurrency: ConcurrencyMode::Serial,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: "dispatch-test-fire".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };
    assert!(matches!(
        engine.dispatch(serial).await,
        FireResult::Fired { .. }
    ));
    assert_eq!(materializer.gate_dids(), vec![expected_did.clone()]);

    let latest_only = FireIntent {
        trigger_id: Some("sched-did".to_string()),
        trigger_kind: TriggerKind::Schedule,
        task,
        concurrency: ConcurrencyMode::LatestOnly,
        event_vars: serde_json::json!({}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: "dispatch-test-fire".to_string(),
        pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    };
    assert!(matches!(
        engine.dispatch(latest_only).await,
        FireResult::Fired { .. }
    ));
    assert_eq!(materializer.supersede_dids(), vec![expected_did]);
}

#[tokio::test]
async fn scheduled_target_uses_stable_receipt_and_actual_session_template_scope() {
    let task = resolved_task("session={{ session.session_id }} request={{ request.request_id }}");
    let mut schedule = resolved_schedule("wake", task.clone());
    schedule.session_id_template = Some("captain-session".into());
    schedule.concurrency = ConcurrencyMode::QueuedSerial;
    let snapshot = snapshot_with_schedules(RapidHashMap::from_iter([("wake".into(), schedule)]));
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: snapshot.agent("general").unwrap().node_did().to_string(),
        trigger_id: "wake".into(),
        source_collection: "Schedule".into(),
        source_doc_id: "persisted-due-occurrence".into(),
    };
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone());
    for fired_at in ["2030-01-01T00:00:00Z", "2030-01-02T00:00:00Z"] {
        let result = engine
            .dispatch(FireIntent {
                trigger_id: Some("wake".into()),
                trigger_kind: TriggerKind::Schedule,
                task: task.clone(),
                concurrency: ConcurrencyMode::QueuedSerial,
                event_vars: serde_json::json!({"fired_at":fired_at}),
                doc_vars: None,
                correlation: None,
                group_vars: None,
                trigger_context: None,
                args_vars: None,
                durable_fire_key: "persisted-due-occurrence".into(),
                pre_materialized_request_id: None,
                on_result: Box::new(|_| {}),
            })
            .await;
        assert!(matches!(result, FireResult::Fired { .. }), "{result:?}");
    }
    let calls = materializer.calls();
    assert_eq!(calls.len(), 2);
    for ((_, kind, prompt), fired_at) in calls
        .into_iter()
        .zip(["2030-01-01T00:00:00Z", "2030-01-02T00:00:00Z"])
    {
        assert_eq!(kind, TriggerKind::Schedule);
        assert_eq!(
            prompt,
            format!(
                "[scheduled trigger:wake · task:t1 · at:{fired_at}]\n\nsession=captain-session request={}",
                identity.request_id()
            )
        );
    }
}
