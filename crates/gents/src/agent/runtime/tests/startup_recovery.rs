use super::support::*;
use super::*;

// These timeouts detect deadlocks; they are not startup latency assertions.
// Leave enough wall-clock headroom for the package-wide suite, which runs many
// embedded DefraDB nodes concurrently on shared CI runners.
const STARTUP_DEADLOCK_GUARD: Duration = Duration::from_secs(30);

struct RejectExecutorDemotionWriter;

struct RejectRouterGenerationRunWriter;

#[derive(Default)]
struct ExhaustStartupSourceWriter {
    source_attempts: std::sync::atomic::AtomicUsize,
    source_exhausted: tokio::sync::Notify,
    persisted: std::sync::Mutex<Vec<BehaviorReadinessSnapshot>>,
}

#[derive(Default)]
struct ExhaustReadyWriter {
    ready_attempts: std::sync::atomic::AtomicUsize,
    persisted: std::sync::Mutex<Vec<BehaviorReadinessSnapshot>>,
}

#[async_trait::async_trait]
impl crate::behavior_readiness_publisher::BehaviorReadinessWriter for RejectExecutorDemotionWriter {
    async fn upsert(
        &self,
        _agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        if snapshot.behaviors.iter().any(|entry| {
            entry.reason == Some(BehaviorReadinessUnavailableReason::ExecutorStartFailed)
        }) {
            return Err(crate::behavior_readiness_publisher::FatalBehaviorReadinessWrite.into());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::behavior_readiness_publisher::BehaviorReadinessWriter
    for RejectRouterGenerationRunWriter
{
    async fn upsert(
        &self,
        _agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        if snapshot.router_generation > 0 {
            return Err(crate::behavior_readiness_publisher::FatalBehaviorReadinessWrite.into());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::behavior_readiness_publisher::BehaviorReadinessWriter for ExhaustStartupSourceWriter {
    async fn upsert(
        &self,
        _agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        if snapshot.process_state == BehaviorReadinessProcessState::Recovering
            && snapshot.active_generation > 0
        {
            let attempt = self
                .source_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if attempt == 5 {
                self.source_exhausted.notify_one();
            }
            anyhow::bail!("injected startup source persistence exhaustion");
        }
        self.persisted
            .lock()
            .expect("startup source writer mutex poisoned")
            .push(snapshot.clone());
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::behavior_readiness_publisher::BehaviorReadinessWriter for ExhaustReadyWriter {
    async fn upsert(
        &self,
        _agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        if snapshot.process_state == BehaviorReadinessProcessState::Ready {
            self.ready_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("injected Ready persistence exhaustion");
        }
        self.persisted
            .lock()
            .expect("Ready writer mutex poisoned")
            .push(snapshot.clone());
        Ok(())
    }
}

async fn wait_for_request_state(
    node: &defra_node::EmbeddedNode,
    doc_id: &str,
    expected_lifecycle_state: &str,
) {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
                    lifecycle_state
                }}
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
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
            .expect("AgentRequest row");
        let lifecycle_state = row
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if lifecycle_state == expected_lifecycle_state {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest {} to reach lifecycle_state={}, last row={:?}",
            doc_id,
            expected_lifecycle_state,
            row
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fetch_backend_probe_row(
    node: &defra_node::EmbeddedNode,
    backend_id: &str,
) -> (String, Option<String>) {
    let escaped_backend_id = escape_graphql_string(backend_id);
    let query = format!(
        r#"{{
            InferenceBackend(filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }}, limit: 1) {{
                probe_status
                last_probe
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "InferenceBackend probe row query failed: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("InferenceBackend row");
    let probe_status = row
        .get("probe_status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let last_probe = row
        .get("last_probe")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    (probe_status, last_probe)
}

async fn wait_for_backend_probe_status(
    node: &defra_node::EmbeddedNode,
    backend_id: &str,
    expected_status: &str,
) -> (String, Option<String>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row = fetch_backend_probe_row(node, backend_id).await;
        if row.0 == expected_status {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for InferenceBackend {backend_id} to reach \
             probe_status={expected_status}, last row={row:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn run_agent_starts_when_startup_probe_cannot_validate_model() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("startup-probe-rejects-model"));
    let mock_endpoint = MockModelEndpoint::start("different-model").unwrap();
    bind_default_behavior_backend_with_capacity_and_probe_status(
        node.as_ref(),
        identity.did(),
        "backend-startup-probe",
        mock_endpoint.endpoint(),
        1,
        "unknown",
    )
    .await;
    let observer = Arc::new(RecordingObserver::default());
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;
    let status = fetch_runtime_status(node.as_ref(), identity.did()).await;
    assert_eq!(status.process_state, "ready");
    assert_eq!(status.reconcile_phase, "idle");
    assert_eq!(status.active_generation, 1);
    assert_eq!(status.last_reconcile_result, "startup");
    assert!(status.last_reconcile_error.is_empty());
    let (probe_status, last_probe) =
        wait_for_backend_probe_status(node.as_ref(), "backend-startup-probe", "healthy").await;
    assert_eq!(probe_status, "healthy");
    assert!(
        last_probe.is_some(),
        "startup unknown -> healthy promotion must stamp document last_probe"
    );

    let _ = shutdown_tx.send(true);
    handle
        .await
        .expect("agent task should join")
        .expect("agent run should return ok");

    let observed = observer
        .states
        .lock()
        .expect("recording observer mutex poisoned")
        .clone();
    assert_eq!(
        observed,
        vec![
            crate::agent::ProcessLifecycleState::Recovering,
            crate::agent::ProcessLifecycleState::Ready,
            crate::agent::ProcessLifecycleState::ShuttingDown,
            crate::agent::ProcessLifecycleState::Shutdown,
        ]
    );
}

#[tokio::test]
async fn run_agent_fails_when_all_behaviors_are_unavailable_due_to_invalid_config() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("startup-invalid-config"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-invalid-config",
        "http://127.0.0.1:9/v1",
    )
    .await;
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());
    let mut default_behavior = crate::load_agent_behavior(node.as_ref(), &default_behavior_id)
        .await
        .unwrap()
        .expect("default behavior document");
    default_behavior.tool_selection_id = Some("missing-tool-selection".to_string());
    crate::upsert_agent_behavior(node.as_ref(), &default_behavior)
        .await
        .unwrap();

    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.behaviors().is_empty());
    assert_eq!(agent.unavailable_behaviors().len(), 1);
    let unavailable_reason = agent
        .unavailable_behaviors()
        .get(&default_behavior_id)
        .expect("default behavior should be unavailable");
    assert!(
        unavailable_reason
            .diagnostic
            .contains("references missing tool selection missing-tool-selection"),
        "unexpected unavailable reason: {}",
        unavailable_reason.diagnostic
    );

    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let error = agent
        .run(shutdown_rx)
        .await
        .expect_err("startup should fail for structurally invalid config");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("no runnable behaviors at startup due to invalid configuration"),
        "unexpected startup error: {error_text}"
    );
    assert!(
        error_text.contains("tool configuration is invalid"),
        "unexpected startup error: {error_text}"
    );
    assert!(
        !error_text.contains("missing-tool-selection"),
        "private configuration diagnostics leaked through startup error: {error_text}"
    );

    let status = fetch_runtime_status(node.as_ref(), identity.did()).await;
    assert_eq!(status.process_state, "shutdown");
    assert_eq!(status.reconcile_phase, "idle");
    assert_eq!(status.active_generation, 0);
    assert_eq!(status.last_reconcile_result, "error");
    assert!(status
        .last_reconcile_error
        .contains("tool configuration is invalid"));
    assert!(!status
        .last_reconcile_error
        .contains("missing-tool-selection"));
    let readiness = fetch_behavior_readiness(node.as_ref(), identity.did()).await;
    assert_eq!(
        readiness.process_state,
        BehaviorReadinessProcessState::Shutdown
    );
    assert_eq!(readiness.behaviors.len(), 1);
    assert_eq!(
        readiness.behaviors[0].reason,
        Some(BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid)
    );
}

#[tokio::test]
async fn demotion_persistence_failure_is_the_exact_run_agent_failure_and_never_admits() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("demotion-persistence-run-agent"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-demotion-persistence",
        mock_endpoint.endpoint(),
    )
    .await;
    let escaped_backend_id = escape_graphql_string("backend-demotion-persistence");
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                input: {{ api_key_env_var: "GENTS_TEST_DEMOTION_PERSISTENCE_UNSET" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert!(std::env::var_os("GENTS_TEST_DEMOTION_PERSISTENCE_UNSET").is_none());

    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            retry_policy: crate::retry::RetryPolicy {
                max_retries: 1,
                base_delay_ms: 1,
                max_delay_ms: 1,
            },
            startup_readiness: crate::startup_readiness::StartupReadinessOptions {
                build_failure_budget: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let request_doc_id = create_agent_request(
        node.as_ref(),
        identity.did(),
        "req-demotion-persistence",
        "session-demotion-persistence",
        "must remain pending",
    )
    .await;
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    let error = tokio::time::timeout(
        STARTUP_DEADLOCK_GUARD,
        super::startup::run_agent_with_readiness_writer(
            agent,
            shutdown_rx,
            Arc::new(RejectExecutorDemotionWriter),
            Duration::from_millis(1),
        ),
    )
    .await
    .expect("fatal demotion persistence must terminate run_agent boundedly")
    .expect_err("fatal demotion persistence cannot be reported as clean shutdown");
    assert_eq!(
        error.root_cause().to_string(),
        "injected fatal behavior readiness write"
    );
    assert!(
        format!("{error:#}").contains("injected fatal behavior readiness write"),
        "runtime returned the wrong failure: {error:#}"
    );
    wait_for_request_state(node.as_ref(), &request_doc_id, "pending").await;
}

#[tokio::test]
async fn router_generation_persistence_failure_terminates_run_agent_before_dispatch() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("router-generation-persistence-run-agent"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-router-generation-persistence",
        mock_endpoint.endpoint(),
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let request_doc_id = create_agent_request(
        node.as_ref(),
        identity.did(),
        "req-router-generation-persistence",
        "session-router-generation-persistence",
        "must remain pending",
    )
    .await;
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    let error = tokio::time::timeout(
        STARTUP_DEADLOCK_GUARD,
        super::startup::run_agent_with_readiness_writer(
            agent,
            shutdown_rx,
            Arc::new(RejectRouterGenerationRunWriter),
            Duration::from_millis(1),
        ),
    )
    .await
    .expect("router-generation persistence failure must terminate run_agent")
    .expect_err("router-generation persistence failure cannot become clean shutdown");
    assert_eq!(
        error.root_cause().to_string(),
        "injected fatal behavior readiness write"
    );
    assert!(
        format!("{error:#}").contains("durably acknowledge router generation"),
        "runtime returned the wrong failure: {error:#}"
    );
    wait_for_request_state(node.as_ref(), &request_doc_id, "pending").await;
}

#[tokio::test]
async fn startup_source_persistence_exhaustion_fails_closed_and_is_the_exact_run_error() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("startup-source-persistence-exhaustion"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-startup-source-persistence",
        mock_endpoint.endpoint(),
    )
    .await;
    let observer = Arc::new(RecordingObserver::default());
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let writer = Arc::new(ExhaustStartupSourceWriter::default());
    let (slot_started_tx, mut slot_started_rx) = mpsc::unbounded_channel();
    let (slot_shutdown_tx, mut slot_shutdown_rx) = mpsc::unbounded_channel();
    let (slot_exited_tx, mut slot_exited_rx) = mpsc::unbounded_channel();
    let slot_exit_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let slot_runner: super::startup::TestSlotRunner = {
        let slot_exit_gate = slot_exit_gate.clone();
        Arc::new(move |generation, mut shutdown| {
            let slot_started_tx = slot_started_tx.clone();
            let slot_shutdown_tx = slot_shutdown_tx.clone();
            let slot_exited_tx = slot_exited_tx.clone();
            let slot_exit_gate = slot_exit_gate.clone();
            Box::pin(async move {
                let _ = slot_started_tx.send(generation);
                let _ = shutdown.changed().await;
                let _ = slot_shutdown_tx.send(generation);
                let permit = slot_exit_gate
                    .acquire_owned()
                    .await
                    .expect("slot exit gate closed");
                permit.forget();
                let _ = slot_exited_tx.send(generation);
                Ok(())
            })
        })
    };
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut run = tokio::spawn(
        super::startup::run_agent_with_readiness_writer_and_slot_runner(
            agent,
            shutdown_rx,
            writer.clone(),
            Duration::from_millis(1),
            Some(slot_runner),
        ),
    );
    assert_eq!(
        tokio::time::timeout(STARTUP_DEADLOCK_GUARD, slot_started_rx.recv())
            .await
            .expect("generation-one slot must start"),
        Some(1)
    );
    tokio::time::timeout(STARTUP_DEADLOCK_GUARD, writer.source_exhausted.notified())
        .await
        .expect("startup source must exhaust all persistence attempts");
    assert_eq!(
        tokio::time::timeout(STARTUP_DEADLOCK_GUARD, slot_shutdown_rx.recv())
            .await
            .expect("slot runner must observe owned startup shutdown"),
        Some(1)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut run)
            .await
            .is_err(),
        "run_agent returned while its generation-one slot was still gated"
    );
    slot_exit_gate.add_permits(1);
    assert_eq!(
        tokio::time::timeout(STARTUP_DEADLOCK_GUARD, slot_exited_rx.recv())
            .await
            .expect("generation-one slot must report exit"),
        Some(1)
    );
    let error = tokio::time::timeout(STARTUP_DEADLOCK_GUARD, run)
        .await
        .expect("startup source exhaustion must terminate run_agent boundedly")
        .expect("run_agent task must join")
        .expect_err("startup source exhaustion cannot become clean shutdown");
    assert_eq!(
        error.root_cause().to_string(),
        "injected startup source persistence exhaustion"
    );
    assert!(
        format!("{error:#}").contains("durably publish startup behavior readiness source"),
        "runtime returned the wrong failure: {error:#}"
    );
    assert_eq!(
        writer
            .source_attempts
            .load(std::sync::atomic::Ordering::SeqCst),
        5,
        "the ordered publisher must exhaust its bounded retry budget"
    );
    let persisted = writer
        .persisted
        .lock()
        .expect("startup source writer mutex poisoned");
    assert!(
        persisted
            .iter()
            .all(|snapshot| snapshot.process_state != BehaviorReadinessProcessState::Ready),
        "failed startup source publication must never persist Ready"
    );
    assert!(
        persisted
            .iter()
            .any(|snapshot| snapshot.process_state == BehaviorReadinessProcessState::Shutdown),
        "publisher must recover after the failed command and flush terminal Shutdown"
    );
    let observed = observer
        .states
        .lock()
        .expect("recording observer mutex poisoned");
    assert!(!observed.contains(&crate::agent::ProcessLifecycleState::Ready));
}

#[tokio::test]
async fn ready_persistence_exhaustion_is_fatal_and_never_opens_runtime_readiness() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("ready-persistence-exhaustion"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-ready-persistence",
        mock_endpoint.endpoint(),
    )
    .await;
    let observer = Arc::new(RecordingObserver::default());
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let writer = Arc::new(ExhaustReadyWriter::default());
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    let error = tokio::time::timeout(
        STARTUP_DEADLOCK_GUARD,
        super::startup::run_agent_with_readiness_writer(
            agent,
            shutdown_rx,
            writer.clone(),
            Duration::from_millis(1),
        ),
    )
    .await
    .expect("Ready persistence exhaustion must terminate run_agent boundedly")
    .expect_err("Ready persistence exhaustion cannot become clean shutdown");
    assert_eq!(
        error.root_cause().to_string(),
        "injected Ready persistence exhaustion"
    );
    assert!(
        format!("{error:#}").contains("durably publish runtime Ready state"),
        "runtime returned the wrong failure: {error:#}"
    );
    assert_eq!(
        writer
            .ready_attempts
            .load(std::sync::atomic::Ordering::SeqCst),
        5,
        "the ordered publisher must exhaust its bounded retry budget"
    );
    let persisted = writer
        .persisted
        .lock()
        .expect("Ready writer mutex poisoned");
    assert!(
        persisted
            .iter()
            .all(|snapshot| snapshot.process_state != BehaviorReadinessProcessState::Ready),
        "a rejected Ready command must not change durable readiness"
    );
    assert!(
        persisted
            .iter()
            .any(|snapshot| snapshot.process_state == BehaviorReadinessProcessState::Shutdown),
        "terminal Shutdown must flush after Ready command exhaustion"
    );
    let observed = observer
        .states
        .lock()
        .expect("recording observer mutex poisoned");
    assert!(!observed.contains(&crate::agent::ProcessLifecycleState::Ready));
}

#[tokio::test]
async fn run_agent_starts_with_all_behaviors_unavailable_and_rejects_requests_at_runtime() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("startup-all-unavailable"));
    bind_default_behavior_backend_with_capacity_and_probe_status(
        node.as_ref(),
        identity.did(),
        "backend-unavailable",
        "http://127.0.0.1:9/v1",
        1,
        "unknown",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.behaviors().is_empty());
    assert_eq!(agent.unavailable_behaviors().len(), 1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;
    let status = fetch_runtime_status(node.as_ref(), identity.did()).await;
    assert_eq!(status.process_state, "ready");
    assert_eq!(status.reconcile_phase, "idle");
    assert_eq!(status.active_generation, 1);
    assert_eq!(status.last_reconcile_result, "startup");
    assert!(status.last_reconcile_error.is_empty());
    let readiness = fetch_behavior_readiness(node.as_ref(), identity.did()).await;
    assert_eq!(
        readiness.process_state,
        BehaviorReadinessProcessState::Ready
    );
    assert_eq!(readiness.behaviors.len(), 1);
    assert_eq!(
        readiness.behaviors[0].state,
        BehaviorReadinessState::Unavailable
    );
    assert_eq!(
        readiness.behaviors[0].reason,
        Some(BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable)
    );

    let request_doc_id = create_agent_request(
        node.as_ref(),
        identity.did(),
        "req-unavailable-runtime",
        "session-unavailable-runtime",
        "hello",
    )
    .await;
    wait_for_request_state(node.as_ref(), &request_doc_id, "failed").await;

    let request_query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                failure_reason
            }}
        }}"#,
        escape_graphql_string(&request_doc_id),
    );
    let request_response = node.execute(&request_query).await;
    assert!(
        !request_response.has_errors(),
        "AgentRequest failure query failed: {:?}",
        request_response.errors
    );
    let failure_reason = request_response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("failure_reason"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        failure_reason, "inference backend is temporarily unavailable",
        "runtime rejection must expose only the typed public readiness reason"
    );

    let _ = shutdown_tx.send(true);
    handle
        .await
        .expect("agent task should join")
        .expect("agent run should return ok");
}

#[tokio::test]
async fn run_agent_recovers_backend_availability_without_restart() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("startup-backend-recovers"));
    bind_default_behavior_backend_with_capacity_and_probe_status(
        node.as_ref(),
        identity.did(),
        "backend-recovers",
        "http://127.0.0.1:9/v1",
        1,
        "unknown",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.behaviors().is_empty());
    assert_eq!(agent.unavailable_behaviors().len(), 1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;
    let startup_status = fetch_runtime_status(node.as_ref(), identity.did()).await;
    assert_eq!(startup_status.active_generation, 1);
    assert_eq!(startup_status.last_reconcile_result, "startup");
    let startup_readiness = fetch_behavior_readiness(node.as_ref(), identity.did()).await;
    assert_eq!(startup_readiness.behaviors.len(), 1);
    assert_eq!(
        startup_readiness.behaviors[0].state,
        BehaviorReadinessState::Unavailable
    );

    update_backend_probe_status(node.as_ref(), "backend-recovers", "healthy").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let status = fetch_runtime_status(node.as_ref(), identity.did()).await;
        let readiness = fetch_behavior_readiness(node.as_ref(), identity.did()).await;
        if status.process_state == "ready"
            && status.active_generation >= 2
            && status.last_reconcile_result == "applied"
            && status.last_reconcile_error.is_empty()
            && readiness.active_generation >= 2
            && readiness.behaviors.len() == 1
            && readiness.behaviors[0].state == BehaviorReadinessState::Ready
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime to recover backend availability; last status: {:?}",
            status
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = shutdown_tx.send(true);
    handle
        .await
        .expect("agent task should join")
        .expect("agent run should return ok");
}

#[tokio::test]
async fn run_agent_shutdown_is_prompt_while_request_waits_for_backend_capacity() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("shutdown-waiting-request"));
    let mock_endpoint = MockModelEndpoint::start_blocking_chat("default").unwrap();
    bind_default_behavior_backend_with_capacity(
        node.as_ref(),
        identity.did(),
        "backend-blocked",
        mock_endpoint.endpoint(),
        1,
    )
    .await;
    let agent = crate::Gents::builder()
        .node(node.clone())
        .identity(identity.clone())
        .default_behavior_id("general")
        .tool_ceiling(ToolCeiling::meta_only())
        .behavior("general")
        .backend_id("backend-blocked")
        .model_name("default")
        .done()
        .behavior("code")
        .backend_id("backend-blocked")
        .model_name("default")
        .done()
        .build()
        .await
        .unwrap();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    let first_request_doc_id = create_agent_request_for_behavior(
        node.as_ref(),
        identity.did(),
        Some("general"),
        "req-shutdown-running",
        "session-shutdown-running",
        "hello",
    )
    .await;
    wait_for_request_state(node.as_ref(), &first_request_doc_id, "processing").await;
    // The property under test starts when the first request owns the sole
    // backend permit. Waiting for the mock server to observe bytes adds the
    // unrelated synchronous capture write and HTTP scheduler to test setup;
    // under the full parallel suite that made this capacity test time out
    // before it reached its actual assertion (#1060).
    wait_for_inference_call_state(node.as_ref(), "req-shutdown-running", "running").await;

    let queued_request_doc_id = create_agent_request_for_behavior(
        node.as_ref(),
        identity.did(),
        Some("code"),
        "req-shutdown-waiting",
        "session-shutdown-waiting",
        "hello",
    )
    .await;
    wait_for_request_state(node.as_ref(), &queued_request_doc_id, "processing").await;
    wait_for_inference_call_state(node.as_ref(), "req-shutdown-waiting", "queued").await;

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("agent shutdown should not wait for backend deadline")
        .expect("agent task should join")
        .expect("agent run should return ok");

    wait_for_request_state(node.as_ref(), &first_request_doc_id, "failed").await;
    wait_for_request_state(node.as_ref(), &queued_request_doc_id, "failed").await;
}

#[tokio::test]
async fn run_agent_shutdown_drains_admission_from_a_full_executor_queue() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("shutdown-full-executor-queue"));
    let mock_endpoint = MockModelEndpoint::start_blocking_chat("default").unwrap();
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-full-executor-queue",
        mock_endpoint.endpoint(),
    )
    .await;
    let (dispatch_probe_tx, mut dispatch_probe_rx) = mpsc::unbounded_channel();
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            router_dispatch_probe: Some(dispatch_probe_tx),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    // One request is held by the blocked worker, 32 fill the bounded executor
    // queue, and the 34th leaves the router blocked in dispatcher.send while
    // holding an admission read lease.
    for index in 0..34 {
        create_agent_request(
            node.as_ref(),
            identity.did(),
            &format!("req-full-executor-queue-{index}"),
            &format!("session-full-executor-queue-{index}"),
            "block",
        )
        .await;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..34 {
            dispatch_probe_rx
                .recv()
                .await
                .expect("runtime dropped dispatch probe before saturating executor queue");
        }
    })
    .await
    .expect("router did not reach the full executor queue send boundary");

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("full executor queue must not deadlock admission shutdown")
        .expect("agent task should join")
        .expect("agent run should return ok");
}
