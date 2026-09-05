use super::support::*;
use super::*;
use crate::agent::runtime::router::RuntimeAdmissionGate;
use crate::behavior_readiness_publisher::{BehaviorReadinessWriter, FatalBehaviorReadinessWrite};
use crate::lean_vocab_test::lean_runtime_reconcile_case;

struct CountingWatcher {
    rx: mpsc::Receiver<anyhow::Result<AgentRequest>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::watcher::Watcher for CountingWatcher {
    async fn next_request(&mut self) -> Option<anyhow::Result<AgentRequest>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.rx.recv().await
    }
}

struct RejectRouterGenerationWriter;

#[async_trait::async_trait]
impl BehaviorReadinessWriter for RejectRouterGenerationWriter {
    async fn upsert(
        &self,
        _agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        if snapshot.router_generation > 0 {
            return Err(FatalBehaviorReadinessWrite.into());
        }
        Ok(())
    }
}

fn routed_snapshot(
    generation: u64,
    dispatcher: mpsc::Sender<AgentRequest>,
) -> Arc<crate::runtime_snapshot::ActiveRuntimeSnapshot> {
    Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation,
        principal: None,
        local_did: String::new(),
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
        dispatchers: HashMap::from([("general".to_string(), dispatcher)]),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    })
}

#[tokio::test]
async fn invalid_execution_origin_route_rejection_terminalizes_without_stopping_router() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:test:invalid-origin-route";
    let response = node
        .execute(
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "invalid-origin-route-request"
                    agent_did: "did:test:invalid-origin-route"
                    behavior_id: "general"
                    session_id: "invalid-origin-route-session"
                    content: "hostile"
                    lifecycle_state: "pending"
                    created_at: "2026-09-03T00:00:00Z"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "create malformed routed request: {:?}",
        response.errors
    );
    let response = node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "invalid-origin-route-request" } }
                    limit: 1
                ) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "lookup malformed routed request: {:?}",
        response.errors
    );
    let doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .expect("malformed request doc id")
        .to_string();
    let mut malformed = request(Some("general"), "invalid-origin-route-session");
    malformed.doc_id = doc_id;
    malformed.request_id = "invalid-origin-route-request".to_string();
    malformed.agent_did = agent_did.to_string();

    super::super::router::fail_routed_request(
        node.clone(),
        agent_did,
        malformed,
        "general",
        "behavior unavailable",
    )
    .await
    .expect("invalid origin must be terminalized, not escape the router");

    let response = node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "invalid-origin-route-request" } }
                    limit: 1
                ) { lifecycle_state failure_reason }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "reload terminal row: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .expect("terminal request row");
    assert_eq!(
        row.get("lifecycle_state").and_then(Value::as_str),
        Some("failed")
    );
    assert!(row
        .get("failure_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| reason.contains("execution_origin")));
    node.shutdown().await;
}

#[tokio::test]
async fn closing_admission_cancels_a_send_to_a_full_executor_queue() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:test:full-executor-queue";
    let (dispatcher, mut executor_rx) = mpsc::channel(1);
    dispatcher
        .send(request(Some("general"), "already-queued"))
        .await
        .unwrap();
    let snapshot = routed_snapshot(1, dispatcher);
    let (_active_tx, active_rx) = watch::channel(snapshot.clone());
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let (request_tx, request_rx) = mpsc::channel(1);
    let (status_owner, status) =
        RuntimeStatusHandle::start_with_unbounded_test_clock(node.clone(), agent_did);
    status.initialize_startup("general").await.unwrap();
    status
        .readiness()
        .register_slot("general", 1)
        .await
        .unwrap();
    status
        .readiness()
        .publish_snapshot(snapshot.as_ref())
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let gate = RuntimeAdmissionGate::closed();
    gate.open().await;

    let mut routed = request(Some("general"), "blocked-dispatch");
    routed.agent_did = agent_did.to_string();
    request_tx.send(Ok(routed)).await.unwrap();
    let gate_for_router = gate.clone();
    let router_node = node.clone();
    let router = tokio::spawn(async move {
        super::super::router::run_router_with_watcher(
            router_node,
            agent_did.to_string(),
            ScriptedWatcher { rx: request_rx },
            active_rx,
            shutdown_rx,
            gate_for_router,
            status,
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(1), gate.wait_for_entry_for_test())
        .await
        .expect("router must hold an admission lease before the close race");
    tokio::time::timeout(Duration::from_secs(1), gate.close())
        .await
        .expect("gate close must cancel the blocked executor send");
    tokio::time::timeout(Duration::from_secs(1), router)
        .await
        .expect("router must leave the blocked send after admission closes")
        .unwrap()
        .unwrap();
    assert_eq!(
        executor_rx.recv().await.unwrap().session_id,
        "already-queued"
    );
    assert!(
        executor_rx.try_recv().is_err(),
        "closed admission leaked work"
    );
    status_owner.close().await.unwrap();
    node.shutdown().await;
}

#[tokio::test]
async fn router_generation_write_failure_closes_admission_before_dequeue_or_dispatch() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:test:router-generation-write-failure";
    let (dispatcher, mut executor_rx) = mpsc::channel(1);
    let snapshot = routed_snapshot(1, dispatcher);
    let (_active_tx, active_rx) = watch::channel(snapshot.clone());
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let (request_tx, request_rx) = mpsc::channel(1);
    let (status_owner, status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        agent_did,
        Arc::new(RejectRouterGenerationWriter),
        Duration::from_millis(1),
    );
    status.initialize_startup("general").await.unwrap();
    status
        .readiness()
        .register_slot("general", 1)
        .await
        .unwrap();
    status
        .readiness()
        .publish_snapshot(snapshot.as_ref())
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let gate = RuntimeAdmissionGate::closed();
    gate.open().await;
    let mut routed = request(Some("general"), "must-remain-pending");
    routed.agent_did = agent_did.to_string();
    request_tx.send(Ok(routed)).await.unwrap();
    let watcher_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let error = super::super::router::run_router_with_watcher(
        node.clone(),
        agent_did.to_string(),
        CountingWatcher {
            rx: request_rx,
            calls: watcher_calls.clone(),
        },
        active_rx,
        shutdown_rx,
        gate.clone(),
        status,
    )
    .await
    .expect_err("router generation persistence failure must stop the router");
    assert!(
        format!("{error:#}").contains("injected fatal behavior readiness write"),
        "unexpected router error: {error:#}"
    );
    assert!(!gate.is_open().await);
    assert_eq!(
        watcher_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "router dequeued a request before its generation was durably acknowledged"
    );
    assert!(executor_rx.try_recv().is_err(), "request reached executor");
    status_owner.close().await.unwrap();
    node.shutdown().await;
}

#[tokio::test]
async fn router_holds_request_during_generation_handoff_and_dispatches_after_alignment() {
    let accept = lean_runtime_reconcile_case("accept_request_after_router_observe");
    assert!(accept.legal);

    let agent_did = "did:test:router-latest-snapshot";
    let initial_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: String::new(),
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
    let admission_gate = RuntimeAdmissionGate::closed();
    admission_gate.open().await;
    let mut admission_rx = admission_gate.subscribe();
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
            &admission_gate,
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
        RuntimeStatusHandle::start_with_unbounded_test_clock(node.clone(), agent_did.to_string());
    runtime_status.initialize_startup("general").await.unwrap();
    runtime_status
        .publish_startup_snapshot(initial_snapshot.as_ref())
        .await
        .unwrap();

    let (_request_tx, request_rx) = mpsc::channel(1);
    let mut watcher = ScriptedWatcher { rx: request_rx };
    let mut active_snapshot = active_rx.borrow().clone();
    let runtime_status_for_router = runtime_status.clone();
    let mut readiness_rx = runtime_status.readiness().subscribe_observation();
    let admission_gate = RuntimeAdmissionGate::closed();
    admission_gate.open().await;
    let mut admission_rx = admission_gate.subscribe();
    let router_task = tokio::spawn(async move {
        wait_for_next_request_with_latest_snapshot(
            agent_did,
            &mut watcher,
            &mut active_snapshot,
            &mut active_rx,
            &mut shutdown_rx,
            &admission_gate,
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
    assert_eq!(row.last_reconcile_result, "startup");

    let query = format!(
        r#"{{
            AgentBehaviorReadiness(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, limit: 1) {{
                snapshot_json
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "AgentBehaviorReadiness router query failed: {:?}",
            response.errors
        );
        let router_generation = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentBehaviorReadiness"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("snapshot_json"))
            .and_then(Value::as_str)
            .and_then(|snapshot| serde_json::from_str::<BehaviorReadinessSnapshot>(snapshot).ok())
            .map(|snapshot| snapshot.router_generation)
            .unwrap_or_default();
        if router_generation == router.post_router_generation as u64 {
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
