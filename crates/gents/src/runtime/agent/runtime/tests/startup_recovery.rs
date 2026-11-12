use super::support::*;
use super::*;
use gents_store::identity::NodeIdentity;

const STARTUP_DEADLOCK_GUARD: Duration = Duration::from_secs(30);

struct RejectRouterGenerationRunWriter;

struct AcceptReadinessWriter;

#[async_trait::async_trait]
impl crate::runtime::node_readiness_publisher::NodeReadinessWriter for AcceptReadinessWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        _snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct ScriptedStartupWriter {
    source: std::sync::Mutex<std::collections::VecDeque<String>>,
    ready: std::sync::Mutex<std::collections::VecDeque<String>>,
    source_attempts: std::sync::atomic::AtomicU64,
    ready_attempts: std::sync::atomic::AtomicU64,
    persisted: std::sync::Mutex<Vec<NodeReadinessSnapshot>>,
}

#[async_trait::async_trait]
impl crate::runtime::node_readiness_publisher::NodeReadinessWriter
    for RejectRouterGenerationRunWriter
{
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        if snapshot.router_generation > 0 {
            return Err(crate::runtime::node_readiness_publisher::FatalNodeReadinessWrite.into());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::runtime::node_readiness_publisher::NodeReadinessWriter for ScriptedStartupWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> anyhow::Result<()> {
        let (script, attempts) = match snapshot.process_state {
            NodeReadinessProcessState::Recovering if snapshot.active_generation > 0 => {
                (&self.source, &self.source_attempts)
            }
            NodeReadinessProcessState::Ready => (&self.ready, &self.ready_attempts),
            _ => {
                self.persisted
                    .lock()
                    .expect("scripted writer mutex poisoned")
                    .push(snapshot.clone());
                return Ok(());
            }
        };
        attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = script
            .lock()
            .expect("scripted writer mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| "ok".to_owned());
        match result.as_str() {
            "ok" => {
                self.persisted
                    .lock()
                    .expect("scripted writer mutex poisoned")
                    .push(snapshot.clone());
                Ok(())
            }
            "fail" => anyhow::bail!("injected startup persistence failure"),
            other => panic!("unmodeled write result {other}"),
        }
    }
}

async fn wait_for_held_request_state(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
    expected: &str,
) {
    let doc_id = escape_graphql_string(doc_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let response = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{doc_id}"}}}}, limit: 1) {{lifecycle_state}} }}"#
        )).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        let [row] = rows.as_slice() else {
            panic!("held Agent cannot read its request")
        };
        if row["lifecycle_state"] == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "request {doc_id} did not reach {expected}: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fetch_backend_probe_row(
    node: &defra_node::Node,
    backend_id: &str,
) -> (String, Option<String>) {
    let escaped_backend_id = escape_graphql_string(backend_id);
    let query = format!(
        r#"{{
            InferenceBackendObservation(filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }}, limit: 1) {{
                probe_status
                last_probe
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "InferenceBackendObservation probe row query failed: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackendObservation"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("InferenceBackendObservation row");
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
    node: &defra_node::Node,
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
async fn run_agent_starts_when_startup_probe_discovers_selected_model() {
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = held_runtime_fixture_with_status(
        "startup-probe-discovers-model",
        "backend-startup-probe",
        mock_endpoint.endpoint(),
        1,
        "unknown",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let observer = Arc::new(RecordingObserver::default());
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
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
    assert!(status.active_generation >= 1);
    assert!(status.last_reconcile_error.is_empty());
    let readiness = fetch_node_readiness(node.as_ref(), identity.did()).await;
    let default_agent_did = fixture.agent.did().to_owned();
    let agent = readiness
        .agents
        .iter()
        .find(|entry| entry.agent_did == default_agent_did)
        .expect("default agent readiness");
    assert_eq!(agent.state, AgentReadinessState::Ready);
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
            crate::runtime::agent::ProcessLifecycleState::Recovering,
            crate::runtime::agent::ProcessLifecycleState::Ready,
            crate::runtime::agent::ProcessLifecycleState::ShuttingDown,
            crate::runtime::agent::ProcessLifecycleState::Shutdown,
        ]
    );
}

async fn delete_tools_document(access: &gents_store::config_client::ConfigAccess, tools_id: &str) {
    let response = access
        .execute(&format!(
            r#"{{ Tools(filter: {{ tools_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
            escape_graphql_string(tools_id),
        ))
        .await
        .unwrap();
    let doc_id = response["data"]["Tools"][0]["_docID"]
        .as_str()
        .expect("Tools document to delete")
        .to_owned();
    let doc_id = doc_id.as_str();
    access
        .transact_write("test.delete_tools_document", |txn| {
            Box::pin(async move { txn.delete_configuration_document("Tools", doc_id).await })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn run_agent_fails_when_all_agents_are_unavailable_due_to_invalid_config() {
    let fixture = held_runtime_fixture(
        "startup-invalid-config",
        "backend-invalid-config",
        "http://127.0.0.1:9/v1",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let default_agent_did = fixture.agent.did().to_owned();
    let context_id = format!("{default_agent_did}:context");
    // A selection edge may only name a Tools document that exists. The dangling
    // state is reached the way it can occur in a node: publish the reference
    // while the document is present, then delete the document.
    write_runtime_documents(
        &fixture.user_access,
        vec![(
            gents_store::collection::Collection::Tools,
            serde_json::json!({"node_did": identity.did(), "tools_id": "missing-tools"}),
        )],
    )
    .await;
    merge_fixture_document(
        &fixture.user_access,
        "test.invalid_context",
        gents_store::collection::Collection::AgentContext,
        identity.did(),
        &context_id,
        serde_json::json!({"tools_id": "missing-tools"}),
    )
    .await;
    delete_tools_document(&fixture.user_access, "missing-tools").await;

    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.agents().is_empty());
    assert_eq!(agent.unavailable_agents().len(), 1);
    let unavailable_reason = agent
        .unavailable_agents()
        .get(&default_agent_did)
        .expect("default agent should be unavailable");
    assert!(
        unavailable_reason.diagnostic.contains("missing-tools"),
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
        error_text.contains("no runnable agents at startup due to invalid configuration"),
        "unexpected startup error: {error_text}"
    );
    assert!(
        error_text.contains("tool configuration is invalid"),
        "unexpected startup error: {error_text}"
    );
    assert!(
        !error_text.contains("missing-tools"),
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
    assert!(!status.last_reconcile_error.contains("missing-tools"));
    let readiness = fetch_node_readiness(node.as_ref(), identity.did()).await;
    assert_eq!(readiness.process_state, NodeReadinessProcessState::Shutdown);
    assert_eq!(readiness.agents.len(), 1);
    assert_eq!(
        readiness.agents[0].reason,
        Some(AgentReadinessUnavailableReason::RuntimeConfigurationInvalid)
    );
}

#[tokio::test]
async fn router_generation_persistence_failure_terminates_run_agent_before_dispatch() {
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = held_runtime_fixture_with_status(
        "router-generation-persistence-run-agent",
        "backend-router-generation-persistence",
        mock_endpoint.endpoint(),
        1,
        "healthy",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        fixture.agent.did().parse().unwrap(),
        fixture.agent.as_ref(),
    )
    .unwrap();
    let request_doc_id = create_held_agent_request(
        &node,
        identity.did(),
        fixture.agent.as_ref(),
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
        "injected fatal agent readiness write"
    );
    assert!(
        format!("{error:#}").contains("durably acknowledge router generation"),
        "runtime returned the wrong failure: {error:#}"
    );
    wait_for_held_request_state(&agent_access, &request_doc_id, "pending").await;
}

#[tokio::test]
async fn slot_drain_failure_reaches_run_agent_after_external_shutdown() {
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = held_runtime_fixture_with_status(
        "slot-drain-failure",
        "backend-slot-drain-failure",
        mock_endpoint.endpoint(),
        1,
        "healthy",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (slot_started_tx, mut slot_started_rx) = mpsc::unbounded_channel();
    let slot_runner: super::startup::TestSlotRunner = Arc::new(move |_generation, mut shutdown| {
        let slot_started_tx = slot_started_tx.clone();
        Box::pin(async move {
            let _ = slot_started_tx.send(());
            shutdown.changed().await?;
            anyhow::bail!("injected slot drain failure")
        })
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(
        super::startup::run_agent_with_readiness_writer_and_slot_runner(
            agent,
            shutdown_rx,
            Arc::new(AcceptReadinessWriter),
            Duration::from_millis(1),
            Some(slot_runner),
        ),
    );
    tokio::time::timeout(STARTUP_DEADLOCK_GUARD, slot_started_rx.recv())
        .await
        .expect("slot worker must start")
        .expect("slot worker start channel closed");
    shutdown_tx.send_replace(true);
    let error = tokio::time::timeout(STARTUP_DEADLOCK_GUARD, run)
        .await
        .expect("slot drain failure must terminate run_agent")
        .expect("run_agent task must join")
        .expect_err("slot drain failure cannot become clean shutdown");
    assert!(
        format!("{error:#}").contains("injected slot drain failure"),
        "run_agent lost the slot drain failure: {error:#}"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn startup_publication_follows_the_lean_persistence_model() {
    use gents_lean_vocab::families::startup_publication_startup;
    for case in gents_lean_vocab::families::startup_publication_startup::cases::<String>() {
        let mock_endpoint = MockModelEndpoint::start("default").unwrap();
        let fixture = held_runtime_fixture_with_status(
            "startup-publication",
            "backend-startup-publication",
            mock_endpoint.endpoint(),
            1,
            "healthy",
        )
        .await;
        let observer = Arc::new(RecordingObserver::default());
        let agent = crate::Gents::from_default_agent_documents(
            fixture.db.node.clone(),
            fixture.db.node_identity.clone(),
            crate::runtime::agent::DocumentRuntimeOptions {
                backend_admission: Some(fixture.admission()),
                tool_ceiling: ToolCeiling::meta_only(),
                process_state_observer: Some(observer.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let writer = Arc::new(ScriptedStartupWriter {
            source: std::sync::Mutex::new(case.input.source_writes.iter().cloned().collect()),
            ready: std::sync::Mutex::new(case.input.ready_writes.iter().cloned().collect()),
            ..Default::default()
        });
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut run = tokio::spawn(super::startup::run_agent_with_readiness_writer(
            agent,
            shutdown_rx,
            writer.clone(),
            Duration::from_millis(1),
        ));
        let deadline = tokio::time::Instant::now() + STARTUP_DEADLOCK_GUARD;
        let result = loop {
            if observer
                .states
                .lock()
                .expect("recording observer mutex poisoned")
                .contains(&crate::runtime::agent::ProcessLifecycleState::Ready)
            {
                shutdown_tx.send_replace(true);
            }
            match tokio::time::timeout(Duration::from_millis(50), &mut run).await {
                Ok(joined) => break joined.expect("run_agent task must join"),
                Err(_) => assert!(
                    tokio::time::Instant::now() < deadline,
                    "startup run did not end boundedly"
                ),
            }
        };
        let failure = match result {
            Ok(()) => None,
            Err(error) => {
                assert_eq!(
                    error.root_cause().to_string(),
                    "injected startup persistence failure",
                    "the run error must be the exhausted write's own error: {error:#}"
                );
                let text = format!("{error:#}");
                if text.contains("durably publish startup agent_config readiness source") {
                    Some("source".to_owned())
                } else if text.contains("durably publish runtime Ready state") {
                    Some("ready".to_owned())
                } else {
                    panic!("runtime returned an unmodeled failure: {text}")
                }
            }
        };
        let attempts = match failure.as_deref() {
            Some("source") => writer
                .source_attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            Some(_) => writer
                .ready_attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            None => 0,
        };
        let persisted = writer
            .persisted
            .lock()
            .expect("scripted writer mutex poisoned")
            .iter()
            .map(|snapshot| snapshot.process_state)
            .collect::<Vec<_>>();
        let ready_observed = observer
            .states
            .lock()
            .expect("recording observer mutex poisoned")
            .contains(&crate::runtime::agent::ProcessLifecycleState::Ready);
        case.check(startup_publication_startup::Output {
            failure,
            attempts,
            ready_persisted: persisted.contains(&NodeReadinessProcessState::Ready),
            shutdown_persisted: persisted.contains(&NodeReadinessProcessState::Shutdown),
            ready_observed,
        });
    }
}

#[tokio::test]
async fn run_agent_starts_with_all_agents_unavailable_and_rejects_requests_at_runtime() {
    let fixture = held_runtime_fixture_with_status(
        "startup-all-unavailable",
        "backend-unavailable",
        "http://127.0.0.1:9/v1",
        1,
        "unknown",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.agents().is_empty());
    assert_eq!(agent.unavailable_agents().len(), 1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;
    let status = fetch_runtime_status(node.as_ref(), identity.did()).await;
    assert_eq!(status.process_state, "ready");
    assert_eq!(status.reconcile_phase, "idle");
    assert_eq!(status.active_generation, 1);
    assert_eq!(status.last_reconcile_result, "startup");
    assert!(status.last_reconcile_error.is_empty());
    let readiness = fetch_node_readiness(node.as_ref(), identity.did()).await;
    assert_eq!(readiness.process_state, NodeReadinessProcessState::Ready);
    assert_eq!(readiness.agents.len(), 1);
    assert_eq!(readiness.agents[0].state, AgentReadinessState::Unavailable);
    assert_eq!(
        readiness.agents[0].reason,
        Some(AgentReadinessUnavailableReason::BackendTemporarilyUnavailable)
    );

    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        fixture.agent.did().parse().unwrap(),
        fixture.agent.as_ref(),
    )
    .unwrap();
    create_user_agent_session(
        &fixture.user_access,
        identity.did(),
        Some(fixture.agent.did()),
        "session-unavailable-runtime",
        fixture.agent.did(),
        Some(fixture.agent.did()),
    )
    .await;
    let request_doc_id = create_held_agent_request(
        &node,
        identity.did(),
        fixture.agent.as_ref(),
        "req-unavailable-runtime",
        "session-unavailable-runtime",
        "hello",
    )
    .await;
    wait_for_held_request_state(&agent_access, &request_doc_id, "failed").await;

    let request_query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                failure_reason
            }}
        }}"#,
        escape_graphql_string(&request_doc_id),
    );
    let request_response = agent_access.execute(&request_query).await.unwrap();
    let failure_reason = request_response["data"]["AgentRequest"][0]["failure_reason"]
        .as_str()
        .unwrap_or_default();
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
    let fixture = held_runtime_fixture_with_status(
        "startup-backend-recovers",
        "backend-recovers",
        "http://127.0.0.1:9/v1",
        1,
        "unknown",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.agents().is_empty());
    assert_eq!(agent.unavailable_agents().len(), 1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;
    let startup_status = fetch_runtime_status(node.as_ref(), identity.did()).await;
    assert_eq!(startup_status.active_generation, 1);
    assert_eq!(startup_status.last_reconcile_result, "startup");
    let startup_readiness = fetch_node_readiness(node.as_ref(), identity.did()).await;
    assert_eq!(startup_readiness.agents.len(), 1);
    assert_eq!(
        startup_readiness.agents[0].state,
        AgentReadinessState::Unavailable
    );

    update_backend_probe_status(&node, identity.did(), "backend-recovers", "healthy").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let status = fetch_runtime_status(node.as_ref(), identity.did()).await;
        let readiness = fetch_node_readiness(node.as_ref(), identity.did()).await;
        if status.process_state == "ready"
            && status.active_generation >= 2
            && status.last_reconcile_result == "applied"
            && status.last_reconcile_error.is_empty()
            && readiness.active_generation >= 2
            && readiness.agents.len() == 1
            && readiness.agents[0].state == AgentReadinessState::Ready
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

async fn submit_capacity_request(
    fixture: &HeldRuntimeFixture,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
) -> String {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_session::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
    let user_did = fixture.user.did();
    let node_did = fixture.db.node_identity.did();
    let create = gents_session::request::lifecycle::build_signed_client_request(
        &fixture.user_access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.to_owned(),
                node_did: node_did.to_owned(),
                requester_did: Some(user_did.to_owned()),
                agent_did: agent_did.to_owned(),
                session_id: session_id.to_owned(),
                content: "hello".to_owned(),
                execution_origin: gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(user_did),
        ),
        RequestSigner::Identity(&fixture.user),
    )
    .await
    .unwrap();
    gents_session::session::update_session_title_with_source(
        &fixture.user_access,
        node_did,
        Some(user_did),
        session_id,
        "capacity shutdown test",
        gents_protocol::session::SessionTitleSource::User,
    )
    .await
    .unwrap();
    gents_session::request::lifecycle::submit_client_request(&fixture.user_access, &create)
        .await
        .unwrap();
    let request = escape_graphql_string(request_id);
    let response = fixture
        .user_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}) {{_docID}} }}"#
        ))
        .await
        .unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    let [row] = rows.as_slice() else {
        panic!("submitted request must be unique")
    };
    row["_docID"].as_str().unwrap().to_owned()
}

async fn wait_for_held_inference_state(
    access: &gents_store::config_client::ConfigAccess,
    request_id: &str,
    expected: &str,
) {
    let request = escape_graphql_string(request_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let response = access.execute(&format!(
            r#"{{ InferenceCall(filter: {{request_id: {{_eq: "{request}"}}}}) {{call_state}} }}"#
        )).await.unwrap();
        let rows = response["data"]["InferenceCall"].as_array().unwrap();
        if rows.iter().any(|row| row["call_state"] == expected) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "inference call {request} did not reach {expected}: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn run_agent_shutdown_is_prompt_while_request_waits_for_backend_capacity() {
    let mock_endpoint = MockModelEndpoint::start_blocking_chat("default").unwrap();
    let fixture = held_runtime_fixture_with_status(
        "shutdown-waiting-request",
        "backend-blocked",
        mock_endpoint.endpoint(),
        1,
        "healthy",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let first_agent = fixture.agent.clone();
    let second_binding = gents_store::identity::provision_agent_identity_authorized(
        &node,
        identity.as_ref(),
        &fixture.user.did().parse().unwrap(),
    )
    .await
    .unwrap();
    let second_agent =
        gents_store::identity::resolve_agent_identity(identity.as_ref(), &second_binding).unwrap();
    grant_held_agent_operations(&node, identity.as_ref(), second_agent.as_ref()).await;
    let first_did = first_agent.did().to_owned();
    let second_did = second_agent.did().to_owned();
    write_runtime_documents(
        &fixture.user_access,
        vec![(
            gents_store::collection::Collection::Agent,
            serde_json::json!({
                "node_did": identity.did(), "agent_did": second_did,
                "creation_nonce": second_binding.creation_nonce,
                "inference_profile_id": format!("{first_did}:inference"),
            }),
        )],
    )
    .await;
    let first_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        first_did.parse().unwrap(),
        first_agent.as_ref(),
    )
    .unwrap();
    let second_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        second_did.parse().unwrap(),
        second_agent.as_ref(),
    )
    .unwrap();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    let first_request_doc_id = submit_capacity_request(
        &fixture,
        &first_did,
        "req-shutdown-running",
        "session-shutdown-running",
    )
    .await;
    wait_for_held_request_state(&fixture.user_access, &first_request_doc_id, "processing").await;
    wait_for_held_inference_state(&first_access, "req-shutdown-running", "running").await;

    let queued_request_doc_id = submit_capacity_request(
        &fixture,
        &second_did,
        "req-shutdown-waiting",
        "session-shutdown-waiting",
    )
    .await;
    wait_for_held_request_state(&fixture.user_access, &queued_request_doc_id, "processing").await;
    wait_for_held_inference_state(&second_access, "req-shutdown-waiting", "queued").await;

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("agent shutdown should not wait for backend deadline")
        .expect("agent task should join")
        .expect("agent run should return ok");

    wait_for_held_request_state(&fixture.user_access, &first_request_doc_id, "failed").await;
    wait_for_held_request_state(&fixture.user_access, &queued_request_doc_id, "failed").await;
    for request_doc_id in [&first_request_doc_id, &queued_request_doc_id] {
        let response = fixture.user_access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ request_id lifecycle_state terminalized_at terminal_output }} }}"#,
            gents_store::graphql::escape_graphql_string(request_doc_id),
        )).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "shutdown must resolve each exact owned request"
        );
        assert_eq!(rows[0]["lifecycle_state"], "failed");
        assert!(rows[0]["terminalized_at"].as_str().is_some());
        let terminal: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(rows[0].clone()).unwrap();
        assert_eq!(
            terminal.terminal_output,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        );
    }
}

#[tokio::test]
async fn generated_snapshot_demotions_preserve_healthy_agents() {
    use gents_automation::triggers::runtime_snapshot::{ResolvedRuntimeSnapshot, UnavailableAgent};
    use rapidhash::{RapidHashMap, RapidHashSet};

    fn ids(value: &[u64]) -> Vec<String> {
        value.iter().map(|id| id.to_string()).collect()
    }

    let endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = held_runtime_fixture_with_status(
        "snapshot-demotion",
        "demotion-backend",
        endpoint.endpoint(),
        1,
        "healthy",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let baseline = startup::snapshot::resolve_startup_snapshot(&agent)
        .await
        .unwrap();
    let template = baseline.agents.values().next().unwrap();
    let template_tools = baseline.tool_configs.values().next().unwrap();
    let surface = baseline.tool_surfaces.values().next().unwrap();
    let cases = &gents_lean_vocab::lean_contract_snapshot().snapshot_demotion_cases;
    assert_eq!(cases.len(), 6);
    for case in cases {
        let runnable = ids(&case.runnable);
        let mut snapshot = ResolvedRuntimeSnapshot::from_parts(
            case.default_agent.to_string(),
            runnable
                .iter()
                .map(|id| {
                    let mut configured = template.as_ref().clone();
                    configured.agent_did = id.clone();
                    gents_tools::surface::AgentWithTools {
                        agent: Arc::new(configured),
                        tools: template_tools.clone(),
                    }
                })
                .collect(),
            runnable
                .iter()
                .map(|id| (id.clone(), surface.clone()))
                .collect(),
            ids(&case.unavailable)
                .into_iter()
                .map(|id| {
                    (
                        id,
                        UnavailableAgent::new(
                            AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
                            "unavailable fixture resource",
                        ),
                    )
                })
                .collect::<RapidHashMap<_, _>>(),
        );
        snapshot.validate_node_readiness_source().unwrap();
        for id in ids(&case.demotions) {
            snapshot.demote_agent(
                &id,
                UnavailableAgent::new(
                    AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
                    "unavailable fixture resource",
                ),
            );
        }
        assert_eq!(
            snapshot.agents.keys().cloned().collect::<RapidHashSet<_>>(),
            ids(&case.post_runnable).into_iter().collect(),
            "{case:?}"
        );
        assert_eq!(
            snapshot
                .tool_surfaces
                .keys()
                .cloned()
                .collect::<RapidHashSet<_>>(),
            ids(&case.post_runnable).into_iter().collect(),
            "{case:?}"
        );
        assert_eq!(
            snapshot
                .unavailable_agents
                .keys()
                .cloned()
                .collect::<RapidHashSet<_>>(),
            ids(&case.post_unavailable).into_iter().collect(),
            "{case:?}"
        );
        assert_eq!(
            snapshot.default_agent_did,
            case.post_default_agent.to_string(),
            "{case:?}"
        );
        assert_eq!(
            snapshot.validate_node_readiness_source().is_ok(),
            case.well_formed,
            "{case:?}"
        );
    }
    node.shutdown().await.expect("node shutdown");
}

async fn write_runtime_documents(
    access: &gents_store::config_client::ConfigAccess,
    documents: Vec<(gents_store::collection::Collection, Value)>,
) {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact_write("test.runtime.configure", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

/// A node with runtime schemas, an identity and a default agent backend
/// bound to the returned model endpoint, which the caller keeps alive.
async fn missing_plugin_node(label: &str) -> (HeldRuntimeFixture, String, MockModelEndpoint) {
    let endpoint = MockModelEndpoint::start("default").unwrap();
    let fixture = held_runtime_fixture(
        &format!("startup-{label}"),
        &format!("backend-{label}"),
        endpoint.endpoint(),
    )
    .await;
    let node_did = fixture.db.node_identity.did().to_owned();
    (fixture, node_did, endpoint)
}

async fn documents_agent(fixture: &HeldRuntimeFixture) -> (crate::Gents, tempfile::TempDir) {
    let plugin_home = tempfile::tempdir().unwrap();
    let agent = crate::Gents::from_default_agent_documents(
        fixture.db.node.clone(),
        fixture.db.node_identity.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (agent, plugin_home)
}

fn agent_document(
    node_did: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    default_agent_did: &str,
) -> (gents_store::collection::Collection, Value) {
    let agent_did = &binding.agent_did;
    (
        gents_store::collection::Collection::Agent,
        serde_json::json!({"node_did": node_did, "agent_did": agent_did,
            "creation_nonce": binding.creation_nonce,
            "context_id": format!("{agent_did}:context"),
            "inference_profile_id": format!("{default_agent_did}:inference")}),
    )
}

fn context_document(
    node_did: &str,
    agent_did: &str,
) -> (gents_store::collection::Collection, Value) {
    (
        gents_store::collection::Collection::AgentContext,
        serde_json::json!({"node_did": node_did, "context_id": format!("{agent_did}:context"),
            "tools_id": format!("{agent_did}:tools")}),
    )
}

fn tools_naming_missing_plugin(
    node_did: &str,
    tools_id: &str,
) -> (gents_store::collection::Collection, Value) {
    (
        gents_store::collection::Collection::Tools,
        serde_json::json!({"node_did": node_did, "tools_id": tools_id,
            "integrations": {"plugins": [{"plugin": "acme/missing_plugin"}]}}),
    )
}

#[tokio::test]
async fn run_agent_starts_without_an_agent_whose_tools_name_a_missing_plugin() {
    let (fixture, node_did, _endpoint) = missing_plugin_node("missing-plugin").await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let default_agent_did = fixture.agent.did().to_owned();
    let broken_binding = gents_store::identity::provision_agent_identity_authorized(
        &node,
        identity.as_ref(),
        &fixture.user.did().parse().unwrap(),
    )
    .await
    .unwrap();
    let broken = broken_binding.agent_did.as_str();
    write_runtime_documents(
        &fixture.user_access,
        vec![
            agent_document(&node_did, &broken_binding, &default_agent_did),
            context_document(&node_did, broken),
            tools_naming_missing_plugin(&node_did, &format!("{broken}:tools")),
        ],
    )
    .await;
    let (agent, _plugin_home) = documents_agent(&fixture).await;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), &node_did, "ready").await;
    let readiness = fetch_node_readiness(node.as_ref(), &node_did).await;
    assert_eq!(readiness.process_state, NodeReadinessProcessState::Ready);
    let entry = |agent_did: &str| {
        readiness
            .agents
            .iter()
            .find(|entry| entry.agent_did == agent_did)
            .unwrap_or_else(|| panic!("{agent_did} readiness in {readiness:?}"))
            .clone()
    };
    assert_eq!(entry(&default_agent_did).state, AgentReadinessState::Ready);
    let broken_entry = entry(broken);
    assert_eq!(broken_entry.state, AgentReadinessState::Unavailable);
    assert_eq!(
        broken_entry.reason,
        Some(AgentReadinessUnavailableReason::ToolSurfaceUnavailable)
    );

    let broken_identity =
        gents_store::identity::resolve_agent_identity(identity.as_ref(), &broken_binding).unwrap();
    grant_held_agent_operations(&node, identity.as_ref(), broken_identity.as_ref()).await;
    let broken_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        broken.parse().unwrap(),
        broken_identity.as_ref(),
    )
    .unwrap();
    create_user_agent_session(
        &fixture.user_access,
        &node_did,
        Some(broken),
        "session-missing-plugin",
        broken,
        Some(broken),
    )
    .await;
    let request_doc_id = create_held_agent_request(
        &node,
        &node_did,
        broken_identity.as_ref(),
        "req-missing-plugin",
        "session-missing-plugin",
        "hello",
    )
    .await;
    wait_for_held_request_state(&broken_access, &request_doc_id, "failed").await;
    let response = broken_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ failure_reason }} }}"#,
            escape_graphql_string(&request_doc_id),
        ))
        .await
        .unwrap();
    assert_eq!(
        response["data"]["AgentRequest"][0]["failure_reason"],
        "tool surface is unavailable"
    );

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STARTUP_DEADLOCK_GUARD, handle)
        .await
        .expect("agent task should join")
        .expect("run task should join")
        .expect("agent run should return ok");
}

#[tokio::test]
async fn run_agent_fails_when_every_agent_names_a_missing_plugin() {
    let (fixture, node_did, _endpoint) = missing_plugin_node("all-missing-plugin").await;
    let tools_id = format!("{}:tools", fixture.agent.did());
    write_runtime_documents(
        &fixture.user_access,
        vec![tools_naming_missing_plugin(&node_did, &tools_id)],
    )
    .await;
    let (agent, _plugin_home) = documents_agent(&fixture).await;

    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let error = agent
        .run(shutdown_rx)
        .await
        .expect_err("startup must refuse when no agent is runnable");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("no runnable agents at startup due to invalid configuration")
            && error_text.contains("tool surface is unavailable"),
        "unexpected startup error: {error_text}"
    );
    assert!(
        !error_text.contains("acme/missing_plugin"),
        "private diagnostics leaked through the startup error: {error_text}"
    );
}

#[tokio::test]
async fn startup_tool_admission_cascade_follows_the_lean_model() {
    use gents_lean_vocab::families::tool_admission_startup_cascade;
    for case in gents_lean_vocab::families::tool_admission_startup_cascade::cases::<
        String,
        AgentReadinessUnavailableReason,
    >() {
        let (fixture, node_did, _endpoint) = missing_plugin_node("demotion-cascade").await;
        let node = fixture.db.node.clone();
        let identity = fixture.db.node_identity.clone();
        let default_agent_did = fixture.agent.did().to_owned();
        let target_binding = gents_store::identity::provision_agent_identity_authorized(
            &node,
            identity.as_ref(),
            &fixture.user.did().parse().unwrap(),
        )
        .await
        .unwrap();
        let caller_binding = gents_store::identity::provision_agent_identity_authorized(
            &node,
            identity.as_ref(),
            &fixture.user.did().parse().unwrap(),
        )
        .await
        .unwrap();
        let target = target_binding.agent_did.as_str();
        let caller = caller_binding.agent_did.as_str();
        let target_tools = match case.input.admission.as_str() {
            "fails_to_build" => tools_naming_missing_plugin(&node_did, &format!("{target}:tools")),
            "builds" => (
                gents_store::collection::Collection::Tools,
                serde_json::json!({"node_did": node_did, "tools_id": format!("{target}:tools")}),
            ),
            other => panic!("unmodeled tool admission {other}"),
        };
        write_runtime_documents(
            &fixture.user_access,
            vec![
                agent_document(&node_did, &target_binding, &default_agent_did),
                context_document(&node_did, target),
                target_tools,
                agent_document(&node_did, &caller_binding, &default_agent_did),
                context_document(&node_did, caller),
                (
                    gents_store::collection::Collection::Tools,
                    serde_json::json!({"node_did": node_did, "tools_id": format!("{caller}:tools"),
                        "agents": {"target_ids": ["target-web-persona"], "enabled": true}}),
                ),
                (
                    gents_store::collection::Collection::AgentTarget,
                    serde_json::json!({"node_did": node_did, "target_id": "target-web-persona",
                        "target_node_did": node_did, "agent_did": target, "name": "web"}),
                ),
                (
                    gents_store::collection::Collection::Task,
                    serde_json::json!({"node_did": node_did, "task_id": "task-web", "agent_did": target,
                        "prompt_template": "tick", "enabled": true}),
                ),
                (
                    gents_store::collection::Collection::Schedule,
                    serde_json::json!({"node_did": node_did, "schedule_id": "schedule-web",
                        "cadence": {"kind": "interval", "interval_secs": 300}}),
                ),
                (
                    gents_store::collection::Collection::Trigger,
                    serde_json::json!({"node_did": node_did, "trigger_id": "trigger-web", "task_id": "task-web",
                        "source": {"kind": "schedule", "schedule_id": "schedule-web"}, "enabled": true}),
                ),
            ],
        )
        .await;
        let (agent, _plugin_home) = documents_agent(&fixture).await;
        let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
            agent.node.clone(),
            agent.mcp_pool.clone(),
            gents_tools::mcp::health::ServiceHealthMap::new(),
            agent.local_hostname.clone(),
            agent.local_subnet.clone(),
            node_did.clone(),
            Some(identity.clone()),
        )
        .with_plugins(agent.plugins().clone());

        let mut snapshot = startup::snapshot::resolve_startup_snapshot(&agent)
            .await
            .unwrap();
        startup::snapshot::validate_startup_snapshot(&agent, &tool_runtime, &mut snapshot)
            .await
            .unwrap();
        assert!(snapshot.agents.contains_key(caller));
        case.check(tool_admission_startup_cascade::CascadeOutput {
            runnable: snapshot.agents.contains_key(target),
            reason: snapshot
                .unavailable_agents
                .get(target)
                .map(|unavailable| unavailable.public_reason),
            offered_as_target: snapshot.tool_surfaces[caller]
                .agent_targets()
                .iter()
                .any(|offered| offered.agent_did == target),
            task_active: snapshot.active_tasks.contains_key("task-web"),
            schedule_active: snapshot.active_schedules.contains_key("trigger-web"),
            schedule_unavailable: snapshot.unavailable_schedules.contains("trigger-web"),
        });
    }
}
