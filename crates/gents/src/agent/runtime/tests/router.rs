use super::support::*;
use super::*;
use crate::lean_vocab_test::lean_runtime_reconcile_case;

#[tokio::test]
async fn router_holds_request_during_generation_handoff_and_dispatches_after_alignment() {
    let accept = lean_runtime_reconcile_case("accept_request_after_router_observe");
    assert!(accept.legal);

    let agent_did = "did:test:router-latest-snapshot";
    let initial_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: String::new(),
        paired_peer_dids: std::collections::HashSet::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: std::collections::HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: std::collections::HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    });
    let updated_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 2,
        principal: None,
        local_did: String::new(),
        paired_peer_dids: std::collections::HashSet::new(),
        default_behavior_id: "code".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: std::collections::HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: std::collections::HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    });
    let (active_tx, mut active_rx) = watch::channel(initial_snapshot);
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let (watcher_tx, watcher_rx) = mpsc::channel(1);
    let mut watcher = ScriptedWatcher { rx: watcher_rx };
    let mut active_snapshot = active_rx.borrow().clone();

    watcher_tx
        .send(Ok(AgentRequest {
            doc_id: "doc-router".to_string(),
            request_id: "req-router".to_string(),
            agent_did: agent_did.to_string(),
            requester_did: None,
            behavior_id: None,
            session_id: "session-router".to_string(),
            content: "hello".to_string(),
            temperature: None,
            top_p: None,
            top_k: None,
            seed: None,
            max_tokens: None,
            max_total_tokens: None,
            metadata: None,
            execution_origin: None,
            created_at: "2026-04-09T00:00:00Z".to_string(),
            deadline: None,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_request_doc_id: None,
            caused_by_parent_tool_call_id: None,
            caused_by_parent_tool_call_doc_id: None,
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_source_doc_id: None,
            caused_by_correlation: None,
            caused_by_trigger_context: None,
            workspace_id: None,
            workspace_authority: None,
            workspace_owner_deployment_id: None,
            workspace_seal_hash: None,
        }))
        .await
        .unwrap();
    let (_admission_tx, mut admission_rx) = watch::channel(true);
    let (_readiness_tx, mut readiness_rx) = watch::channel(
        crate::behavior_readiness_publisher::BehaviorAdmissionObservation::for_test(2, []),
    );
    let (request, routed_snapshot, routed_observation) = {
        let wait = wait_for_next_request_with_latest_snapshot(
            agent_did,
            &mut watcher,
            &mut active_snapshot,
            &mut active_rx,
            &mut shutdown_rx,
            &mut admission_rx,
            &mut readiness_rx,
            None,
        );
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut wait)
                .await
                .is_err(),
            "router returned a request while the source was ahead of the active generation"
        );
        active_tx.send(updated_snapshot).unwrap();
        wait.await
            .expect("router wait should succeed")
            .expect("request should be returned")
    };

    assert_eq!(request.request_id, "req-router");
    assert_eq!(routed_observation.source_generation(), 2);
    assert_eq!(routed_snapshot.generation, 2);
    assert_eq!(routed_snapshot.default_behavior_id, "code");
    assert_eq!(
        active_snapshot.generation,
        accept.post_router_generation as u64
    );
    assert_eq!(active_snapshot.default_behavior_id, "code");
}

#[tokio::test(start_paused = true)]
async fn router_publishes_observed_generation_without_waiting_for_request() {
    let router = lean_runtime_reconcile_case("router_observe_published_generation");
    assert!(router.legal);

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:test:router-observed-generation";
    let initial_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: String::new(),
        paired_peer_dids: std::collections::HashSet::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::from([(
            "general".to_string(),
            crate::runtime_snapshot::UnavailableBehavior::new(
                gents_protocol::row::BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid,
                "test fixture has no executor",
            ),
        )]),
        active_schedules: HashMap::new(),
        unavailable_schedules: std::collections::HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: std::collections::HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    });
    let updated_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 2,
        principal: None,
        local_did: String::new(),
        paired_peer_dids: std::collections::HashSet::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::from([(
            "general".to_string(),
            crate::runtime_snapshot::UnavailableBehavior::new(
                gents_protocol::row::BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid,
                "test fixture has no executor",
            ),
        )]),
        active_schedules: HashMap::new(),
        unavailable_schedules: std::collections::HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: std::collections::HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    });
    let (active_tx, mut active_rx) = watch::channel(initial_snapshot.clone());
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let (runtime_status_owner, runtime_status) =
        RuntimeStatusHandle::start(node.clone(), agent_did.to_string());
    runtime_status.initialize_startup("general").await.unwrap();
    runtime_status
        .publish_startup_snapshot(initial_snapshot.as_ref())
        .await;

    let (_request_tx, request_rx) = mpsc::channel(1);
    let mut watcher = ScriptedWatcher { rx: request_rx };
    let mut active_snapshot = active_rx.borrow().clone();
    let runtime_status_for_router = runtime_status.clone();
    let mut readiness_rx = runtime_status.readiness().subscribe_observation();
    let (_admission_tx, mut admission_rx) = watch::channel(true);
    let router_task = tokio::spawn(async move {
        wait_for_next_request_with_latest_snapshot(
            agent_did,
            &mut watcher,
            &mut active_snapshot,
            &mut active_rx,
            &mut shutdown_rx,
            &mut admission_rx,
            &mut readiness_rx,
            Some(&runtime_status_for_router),
        )
        .await
    });

    tokio::task::yield_now().await;
    runtime_status
        .readiness()
        .publish_snapshot(updated_snapshot.as_ref())
        .await
        .unwrap();
    active_tx.send(updated_snapshot).unwrap();
    tokio::task::yield_now().await;

    let row = fetch_runtime_status(node.as_ref(), agent_did).await;
    assert_eq!(row.active_generation, router.pre_router_generation as i64);
    assert_eq!(row.last_reconcile_result, "startup");

    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, limit: 1) {{
                router_generation
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "AgentRuntime router query failed: {:?}",
            response.errors
        );
        let router_generation = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("router_generation"))
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if router_generation == router.post_router_generation as i64 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "router generation did not advance to {}; last value={router_generation}",
            router.post_router_generation
        );
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(10)).await;
    }

    let _ = shutdown_tx.send(true);
    assert!(router_task.await.unwrap().unwrap().is_none());
    runtime_status_owner.close().await.unwrap();
}
