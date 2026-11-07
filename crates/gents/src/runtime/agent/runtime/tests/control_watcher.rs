use super::support::*;
use super::*;
use crate::runtime::status::{ReconcilePhase, RECONCILE_PHASE_EVENT_TARGET};
use crate::store::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET;
use crate::store::identity::NodeIdentity;
use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Default)]
struct RuntimeViewLoadCapture {
    count: Arc<AtomicUsize>,
}

#[derive(Default)]
struct RuntimeViewLoadField(bool);

impl Visit for RuntimeViewLoadField {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "operation" {
            self.0 = value == "load_runtime_document_view";
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S> Layer<S> for RuntimeViewLoadCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: LayerContext<'_, S>) {
        if event.metadata().target() != WRITE_ATTEMPT_EVENT_TARGET {
            return;
        }
        let mut operation = RuntimeViewLoadField::default();
        event.record(&mut operation);
        if operation.0 {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Default)]
struct ReconcilePhaseCapture {
    phases: Arc<StdMutex<Vec<String>>>,
}

impl ReconcilePhaseCapture {
    fn announced(&self) -> Vec<String> {
        self.phases.lock().expect("phase capture").clone()
    }
}

#[derive(Default)]
struct ReconcilePhaseField(Option<String>);

impl Visit for ReconcilePhaseField {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "reconcile_phase" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S> Layer<S> for ReconcilePhaseCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: LayerContext<'_, S>) {
        if event.metadata().target() != RECONCILE_PHASE_EVENT_TARGET {
            return;
        }
        let mut phase = ReconcilePhaseField::default();
        event.record(&mut phase);
        if let Some(phase) = phase.0 {
            self.phases.lock().expect("phase capture").push(phase);
        }
    }
}

const TEST_CONTROL_WATCHER_TIMING: ControlWatcherTiming = ControlWatcherTiming {
    debounce: Duration::from_millis(20),
    local_debounce: Duration::from_millis(20),
    settle_retry: Duration::from_millis(10),
    settle_window: Duration::from_millis(200),
    idle_sleep: Duration::from_secs(60),
};

const PROPOSAL_TIMEOUT: Duration = Duration::from_secs(5);

async fn run_test_control_watcher(inputs: ControlWatcherInputs) -> Result<()> {
    run_control_watcher_with_timing(inputs, TEST_CONTROL_WATCHER_TIMING).await
}

fn test_tool_runtime(
    node: &Arc<defra_node::Node>,
    node_did: &str,
    resolve_context: &crate::runtime::agent::DocumentResolveContext,
) -> crate::tools::surface::ToolRuntimeContext {
    crate::tools::surface::ToolRuntimeContext::new_with_node_did(
        node.clone(),
        crate::tools::mcp::pool::McpPool::default(),
        crate::tools::mcp::health::ServiceHealthMap::default(),
        "localhost",
        None,
        node_did,
        Some(resolve_context.identity.clone()),
    )
    .with_plugins(resolve_context.plugins.clone())
}

async fn update_node_enabled(access: &crate::ConfigAccess, node_did: &str, enabled: bool) {
    merge_fixture_document(
        access,
        "test.node.enabled",
        crate::Collection::Node,
        node_did,
        node_did,
        serde_json::json!({"enabled": enabled}),
    )
    .await;
}

async fn write_documents(
    access: &crate::ConfigAccess,
    operation: &'static str,
    documents: Vec<(crate::Collection, serde_json::Value)>,
) {
    let documents = documents
        .into_iter()
        .map(
            |(collection, value)| crate::store::config_client::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect();
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(documents).unwrap();
    access
        .transact_write(operation, |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn control_watcher_publishes_reconciled_snapshot_after_relevant_update() {
    crate::test_support::enable_scoped_event_capture();
    let fixture = held_runtime_fixture(
        "control-watcher",
        "backend-control",
        "http://127.0.0.1:8111/v1",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let user_access = fixture.user_access.clone();
    let agent_access = fixture.agent_config_access().await;
    assert_ne!(fixture.user.did(), identity.did());
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
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.node_did().to_string());
    runtime_status
        .initialize_startup(agent.default_agent_did())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let reloads = RuntimeViewLoadCapture::default();
    let reload_count = Arc::clone(&reloads.count);
    let phases = ReconcilePhaseCapture::default();
    let announced_phases = phases.clone();
    let subscriber = Registry::default().with(reloads).with(phases);

    // DefraDB subscriptions are live-only.
    let subscription = subscribe_control_changes(&node).unwrap();
    let context = serde_json::json!({
        "node_did": agent.node_did(),
        "context_id": format!("{}:context", agent.default_agent_did()),
        "tools_id": format!("{}:tools", agent.default_agent_did()),
        "system_prompt": "updated prompt"
    });
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::AgentContext,
            add: context.clone(),
            update: context,
        },
    ])
    .unwrap();
    user_access
        .transact_write("test.context.update", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();

    let watcher_task = tokio::spawn(
        run_test_control_watcher(ControlWatcherInputs {
            node: node.clone(),
            backend_admission: agent.backend_admission.clone(),
            subscription,
            node_did: agent.node_did().to_string(),
            tool_runtime: test_tool_runtime(&node, agent.node_did(), &resolve_context),
            resolve_context,
            proposals_tx: proposal_tx,
            runtime_status: runtime_status.clone(),
            health_events_rx: mpsc::channel::<()>(1).1,
            shutdown: shutdown_rx,
        })
        .with_subscriber(subscriber),
    );

    let snapshot = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("an observed control update must reach the reconcile owner")
        .expect("reconciled snapshot");
    assert_eq!(
        snapshot
            .agents
            .get(agent.default_agent_did())
            .expect("default agent in snapshot")
            .system_prompt,
        "updated prompt"
    );
    assert_eq!(
        announced_phases.announced(),
        ["debouncing", "resolving"],
        "an observed control update debounces before it resolves"
    );
    let resolving = fetch_runtime_status(node.as_ref(), agent.node_did()).await;
    assert_eq!(resolving.reconcile_phase, "resolving");

    runtime_status
        .set_reconcile_phase(ReconcilePhase::Idle)
        .await;
    let settled = fetch_runtime_status(node.as_ref(), agent.node_did()).await;
    let loads_after_reconcile = reload_count.load(Ordering::Relaxed);
    assert!(
        loads_after_reconcile > 0,
        "runtime-view telemetry was captured"
    );
    tokio::time::sleep(TEST_CONTROL_WATCHER_TIMING.settle_retry * 5).await;
    tokio::task::yield_now().await;
    let retried = fetch_runtime_status(node.as_ref(), agent.node_did()).await;
    assert_eq!(retried.reconcile_phase, "idle");
    assert_eq!(retried.updated_at, settled.updated_at);
    assert_eq!(
        reload_count.load(Ordering::Relaxed),
        loads_after_reconcile,
        "a visible reconcile must quiesce instead of polling the full runtime view"
    );

    let context_id = crate::store::graphql::escape_graphql_string(&format!(
        "{}:context",
        agent.default_agent_did()
    ));
    let observed = user_access
        .execute(&format!(
            r#"{{ AgentContext(filter: {{context_id: {{_eq: "{context_id}"}}}}) {{_docID}} }}"#
        ))
        .await
        .unwrap();
    let rows = observed["data"]["AgentContext"].as_array().unwrap();
    let [row] = rows.as_slice() else {
        panic!("fixture context must be unique")
    };
    let doc = escape_graphql_string(row["_docID"].as_str().unwrap());
    let mutation = format!(
        r#"mutation {{ update_AgentContext(docID: "{doc}", input: {{display_name: "Retagged context"}}) {{_docID}} }}"#
    );
    agent_access
        .write("test.context.metadata", &mutation)
        .await
        .unwrap();
    let repeated = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("metadata write must reach reconcile owner")
        .expect("proposal channel");
    assert_eq!(
        repeated.configuration_fingerprint(),
        snapshot.configuration_fingerprint()
    );
    assert_eq!(
        announced_phases.announced(),
        ["debouncing", "resolving", "debouncing", "resolving"],
        "a metadata-only observation debounces before it resolves"
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_reconciles_a_local_write_without_the_replication_debounce() {
    let fixture = held_runtime_fixture(
        "control-watcher-local",
        "backend-control",
        "http://127.0.0.1:8111/v1",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let agent_access = fixture.agent_config_access().await;
    assert_ne!(fixture.user.did(), identity.did());
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
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.node_did().to_string());
    runtime_status
        .initialize_startup(agent.default_agent_did())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let subscription = subscribe_control_changes(&node).unwrap();
    let watcher_task = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: node.clone(),
            backend_admission: agent.backend_admission.clone(),
            subscription,
            node_did: agent.node_did().to_string(),
            tool_runtime: test_tool_runtime(&node, agent.node_did(), &resolve_context),
            resolve_context,
            proposals_tx: proposal_tx,
            runtime_status,
            health_events_rx: mpsc::channel::<()>(1).1,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            debounce: Duration::from_secs(3600),
            ..TEST_CONTROL_WATCHER_TIMING
        },
    ));

    write_documents(
        &agent_access,
        "test.context.local",
        vec![(
            crate::Collection::AgentContext,
            serde_json::json!({
                "node_did": agent.node_did(),
                "context_id": format!("{}:context", agent.default_agent_did()),
                "tools_id": format!("{}:tools", agent.default_agent_did()),
                "system_prompt": "operator prompt"
            }),
        )],
    )
    .await;

    let snapshot = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("a local write must reconcile after the local debounce")
        .expect("reconciled snapshot");
    assert_eq!(
        snapshot
            .agents
            .get(agent.default_agent_did())
            .expect("default agent in snapshot")
            .system_prompt,
        "operator prompt"
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_demotes_and_recovers_agent_on_measured_health_flip() {
    crate::test_support::enable_scoped_event_capture();
    let fixture = held_runtime_fixture(
        "control-watcher-health",
        "backend-measured",
        "http://127.0.0.1:8113/v1",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    assert_ne!(fixture.user.did(), identity.did());
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
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let backend_health = agent.backend_health();
    let agent_did = agent.default_agent_did().to_string();
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.node_did().to_string());
    runtime_status
        .initialize_startup(agent.default_agent_did())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let (health_tx, health_rx) = mpsc::channel::<()>(1);
    let phases = ReconcilePhaseCapture::default();
    let announced_phases = phases.clone();
    let subscriber = Registry::default().with(phases);

    let watcher_task = tokio::spawn(
        run_test_control_watcher(ControlWatcherInputs {
            node: node.clone(),
            backend_admission: agent.backend_admission.clone(),
            subscription: subscribe_control_changes(&node).unwrap(),
            node_did: agent.node_did().to_string(),
            tool_runtime: test_tool_runtime(&node, agent.node_did(), &resolve_context),
            resolve_context,
            proposals_tx: proposal_tx,
            runtime_status: runtime_status.clone(),
            health_events_rx: health_rx,
            shutdown: shutdown_rx,
        })
        .with_subscriber(subscriber),
    );

    tokio::task::yield_now().await;

    backend_health
        .set_for_test(
            "backend-measured",
            crate::inference::backends::health::BackendHealthState::Unhealthy,
            3,
        )
        .await;
    health_tx.send(()).await.unwrap();

    let snapshot = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("a measured-unhealthy transition must reach the reconcile owner")
        .expect("demotion snapshot");
    assert_eq!(
        announced_phases.announced(),
        ["debouncing", "resolving"],
        "a measured-health transition debounces before it resolves"
    );
    assert!(
        !snapshot.agents.contains_key(&agent_did),
        "agent on a measured-unhealthy backend must leave the active set"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&agent_did)
        .expect("unavailable reason for demoted agent");
    assert_eq!(
        reason.public_reason,
        AgentReadinessUnavailableReason::BackendTemporarilyUnavailable
    );
    let config = snapshot
        .backend_admission_configs
        .get("backend-measured")
        .expect("admission config for measured backend");
    assert!(config.measured_unhealthy);

    backend_health
        .set_for_test(
            "backend-measured",
            crate::inference::backends::health::BackendHealthState::Healthy,
            0,
        )
        .await;
    health_tx.send(()).await.unwrap();

    let snapshot = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("a measured-healthy transition must reach the reconcile owner")
        .expect("recovery snapshot");
    assert_eq!(
        announced_phases.announced(),
        ["debouncing", "resolving", "debouncing", "resolving"],
        "recovery debounces before it resolves"
    );
    assert!(
        snapshot.agents.contains_key(&agent_did),
        "agent must return to the active set after recovery"
    );
    assert!(
        !snapshot
            .backend_admission_configs
            .get("backend-measured")
            .expect("admission config after recovery")
            .measured_unhealthy
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_recovers_after_resolve_error() {
    crate::test_support::enable_scoped_event_capture();
    let fixture = held_runtime_fixture(
        "control-watcher-recover",
        "backend-control-recover",
        "http://127.0.0.1:8112/v1",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let user_access = fixture.user_access.clone();
    assert_ne!(fixture.user.did(), identity.did());
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
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.node_did().to_string());
    runtime_status
        .initialize_startup(agent.default_agent_did())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let reloads = RuntimeViewLoadCapture::default();
    let reload_count = Arc::clone(&reloads.count);
    let subscriber = Registry::default().with(reloads);

    let watcher_task = tokio::spawn(
        run_test_control_watcher(ControlWatcherInputs {
            node: node.clone(),
            backend_admission: agent.backend_admission.clone(),
            subscription: subscribe_control_changes(&node).unwrap(),
            node_did: agent.node_did().to_string(),
            tool_runtime: test_tool_runtime(&node, agent.node_did(), &resolve_context),
            resolve_context,
            proposals_tx: proposal_tx,
            runtime_status: runtime_status.clone(),
            health_events_rx: mpsc::channel::<()>(1).1,
            shutdown: shutdown_rx,
        })
        .with_subscriber(subscriber),
    );

    tokio::task::yield_now().await;
    update_node_enabled(&user_access, agent.node_did(), false).await;

    let failed_status =
        wait_for_runtime_reconcile_phase(node.as_ref(), agent.node_did(), "idle").await;
    assert_eq!(failed_status.active_generation, 0);
    assert_eq!(failed_status.last_reconcile_result, "error");
    assert!(!failed_status.last_reconcile_error.is_empty());
    assert!(proposal_rx.try_recv().is_err());
    assert!(
        reload_count.load(Ordering::Relaxed) > 1,
        "a transient resolution failure must retry during the settle window"
    );

    update_node_enabled(&user_access, agent.node_did(), true).await;

    let snapshot = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("a re-enabled Node must reach the reconcile owner")
        .expect("recovered snapshot");
    assert_eq!(snapshot.default_agent_did, agent.default_agent_did());

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_resolves_context_tools_into_reconciled_tool_surface() {
    let fixture = held_runtime_fixture(
        "control-watcher-tools",
        "backend-control-tools",
        "http://127.0.0.1:8113/v1",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let user_access = fixture.user_access.clone();
    assert_ne!(fixture.user.did(), identity.did());
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.node_did().to_string());
    runtime_status
        .initialize_startup(agent.default_agent_did())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);

    let watcher_task = tokio::spawn(run_test_control_watcher(ControlWatcherInputs {
        node: node.clone(),
        backend_admission: agent.backend_admission.clone(),
        subscription: subscribe_control_changes(&node).unwrap(),
        node_did: agent.node_did().to_string(),
        tool_runtime: test_tool_runtime(&node, agent.node_did(), &resolve_context),
        resolve_context,
        proposals_tx: proposal_tx,
        runtime_status: runtime_status.clone(),
        health_events_rx: mpsc::channel::<()>(1).1,
        shutdown: shutdown_rx,
    }));

    tokio::task::yield_now().await;

    // Under document ACP the WorkspaceRoot policy is always explicit, so host
    // tools need a User-published root the Agent is granted to read.
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let root_text = root_path.to_string_lossy().into_owned();
    crate::tools::surface::set_workspace_root_with_readers(
        &user_access,
        agent.node_did(),
        &root_text,
        None,
        true,
        &[agent.default_agent_did().to_owned()],
    )
    .await
    .unwrap();
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{}:tools", agent.default_agent_did()),
        "node_did": agent.node_did(),
        "display_name": "Read tools",
        "host": {"files": {"mode": "ReadOnly"}, "root": root_text}
    }))
    .unwrap();
    crate::store::config_client::write_tools_document(&user_access, &tools)
        .await
        .unwrap();

    tokio::task::yield_now().await;
    tokio::time::sleep(TEST_CONTROL_WATCHER_TIMING.debounce + Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    // The WorkspaceRoot and Tools writes are separate control changes; under
    // load the debounce can propose the root alone first. Take the first
    // proposal that reflects the published Tools document.
    let tool_names = tokio::time::timeout(PROPOSAL_TIMEOUT, async {
        loop {
            let snapshot = proposal_rx.recv().await.expect("reconciled snapshot");
            let tool_names = snapshot
                .tool_surfaces
                .get(agent.default_agent_did())
                .expect("default agent tool surface")
                .tool_names();
            if tool_names.contains(&"read_file".to_string()) {
                break tool_names;
            }
        }
    })
    .await
    .expect("the published Tools document reaches the reconciled tool surface");
    assert!(tool_names.contains(&"list_files".to_string()));
    assert!(!tool_names.contains(&"discover_tools".to_string()));

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_settles_when_an_unselected_agent_is_permanently_invalid() {
    let fixture = held_runtime_fixture(
        "control-watcher-settle",
        "backend-settle",
        "http://127.0.0.1:8114/v1",
    )
    .await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let user_access = fixture.user_access.clone();
    assert_ne!(fixture.user.did(), identity.did());
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
    let node_did = agent.node_did().to_string();
    let selected_agent_did = agent.default_agent_did().to_string();

    let backend = crate::inference::backends::registry::lookup_backend(
        node.as_ref(),
        &node_did,
        "backend-settle",
    )
    .await
    .unwrap()
    .expect("configured backend document");
    let advertised = |model_name: &str| crate::documents::AdvertisedModel {
        model_name: model_name.into(),
        display_name: None,
        context_window: None,
        max_context_window: None,
        max_output_tokens: None,
        reasoning_efforts: None,
    };
    let catalog = |observed_at: &str, models: Vec<crate::documents::AdvertisedModel>| {
        crate::documents::BackendModelCatalog {
            node_did: None,
            observed_at: observed_at.into(),
            models,
        }
    };
    let runtime_access = crate::ConfigAccess::local_as(node.clone(), node_did.parse().unwrap());
    let first_catalog = catalog(
        "2026-01-01T00:00:00Z",
        vec![advertised("default"), advertised("retired-model")],
    );
    runtime_access
        .transact_write("test.catalog.initial", |txn| {
            let catalog = first_catalog.clone();
            let backend = &backend;
            Box::pin(async move {
                crate::inference::backends::registry::record_model_catalog_in_txn(
                    txn, backend, catalog,
                )
                .await
            })
        })
        .await
        .unwrap();

    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did.clone());
    runtime_status
        .initialize_startup(&selected_agent_did)
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);

    let subscription = subscribe_control_changes(&node).unwrap();
    let spare = crate::store::identity::provision_agent_identity_authorized(
        &node,
        fixture.db.node_identity.as_ref(),
        &fixture.user.did().parse().unwrap(),
    )
    .await
    .unwrap();
    let spare_agent_did = spare.agent_did.clone();
    let spare_profile_id = format!("{spare_agent_did}:inference");
    write_documents(
        &user_access,
        "test.settle.spare",
        vec![
            (
                crate::Collection::InferenceProfile,
                serde_json::json!({
                    "node_did": node_did,
                    "profile_id": spare_profile_id,
                    "backend_id": "backend-settle",
                    "model_name": "retired-model"
                }),
            ),
            (
                crate::Collection::Agent,
                serde_json::json!({
                    "node_did": node_did,
                    "agent_did": spare_agent_did,
                    "creation_nonce": spare.creation_nonce,
                    "inference_profile_id": spare_profile_id
                }),
            ),
        ],
    )
    .await;
    let final_catalog = catalog("2026-01-02T00:00:00Z", vec![advertised("default")]);
    runtime_access
        .transact_write("test.catalog.final", |txn| {
            let catalog = final_catalog.clone();
            let backend = &backend;
            Box::pin(async move {
                crate::inference::backends::registry::record_model_catalog_in_txn(
                    txn, backend, catalog,
                )
                .await
            })
        })
        .await
        .unwrap();

    let watcher_task = tokio::spawn(run_test_control_watcher(ControlWatcherInputs {
        node: node.clone(),
        backend_admission: agent.backend_admission.clone(),
        subscription,
        node_did: node_did.clone(),
        tool_runtime: test_tool_runtime(&node, &node_did, &resolve_context),
        resolve_context,
        proposals_tx: proposal_tx,
        runtime_status: runtime_status.clone(),
        health_events_rx: mpsc::channel::<()>(1).1,
        shutdown: shutdown_rx,
    }));

    let snapshot = tokio::time::timeout(Duration::from_secs(5), proposal_rx.recv())
        .await
        .expect("a permanently invalid unselected agent must not hold reconciliation")
        .expect("reconciled snapshot");
    assert!(snapshot.agents.contains_key(&selected_agent_did));
    assert!(!snapshot.agents.contains_key(&spare_agent_did));
    assert_eq!(
        snapshot
            .unavailable_agents
            .get(&spare_agent_did)
            .expect("unavailable reason for the invalid agent")
            .public_reason,
        AgentReadinessUnavailableReason::InferenceProfileInvalid
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();

    let view = crate::runtime::agent::document_view::load_document_runtime_view(&node, &node_did)
        .await
        .unwrap();
    assert!(
        !view.has_unresolved_agent_references(),
        "permanently invalid inference selection is not a pending document: {:?}",
        view.pending_visibility_details()
    );
}

async fn record_plugin_install(
    access: &crate::store::config_client::ConfigAccess,
    node_binding: &crate::store::config_client::ConfigurationNodeBinding,
    node_did: &str,
    plugin_home: &std::path::Path,
) {
    let identity = crate::pack::PackIdentity {
        installation_doc_id: None,
        agent_bindings: Default::default(),
        coordinate: "fixture/list_files".to_owned(),
        version: "0.1.0".to_owned(),
        digest: "sha256:pack-digest".to_owned(),
        plugins: vec![crate::pack::InstalledPackPlugin {
            name: "list_files".to_owned(),
            digest: installed_record().digest,
        }],
        dependencies: Vec::new(),
        explicit: true,
    };
    crate::pack::record_plugin_store_change_as_with_node_binding(
        access,
        node_did,
        access
            .actor_did()
            .map(identity::Did::new)
            .transpose()
            .unwrap(),
        plugin_home,
        &identity.coordinate,
        Some(&identity),
        Some(node_binding),
    )
    .await
    .unwrap();
}

async fn write_tools_naming_plugin(
    access: &crate::store::config_client::ConfigAccess,
    node_did: &str,
    tools_id: &str,
) {
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": tools_id,
        "node_did": node_did,
        "integrations": {"plugins": [{"plugin": "fixture/list_files"}]},
    }))
    .unwrap();
    crate::store::config_client::write_tools_document(access, &tools)
        .await
        .unwrap();
}

fn installed_record() -> crate::plugin::store::InstalledPlugin {
    serde_json::from_value(serde_json::json!({
        "namespace": "fixture",
        "name": "list_files",
        "version": "0.1.0",
        "digest": format!("sha256:{}", "b".repeat(64)),
        "language": "rust",
        "declaration": {
            "name": "list_files",
            "description": "lists files",
            "artifact": "plugins/list_files.afb",
            "language": "rust",
            "input_schema": {"type": "object"}
        }
    }))
    .unwrap()
}

#[tokio::test]
async fn control_watcher_proposes_a_new_fingerprint_when_the_named_plugin_installs() {
    let fixture = plugin_runtime_fixture(
        "control-watcher-plugin-install",
        "backend-plugin-install",
        "http://127.0.0.1:8115/v1",
    )
    .await
    .unwrap();
    let node = fixture.node.clone();
    let identity = fixture.runtime.clone();
    let plugin_home = tempfile::tempdir().unwrap();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let node_did = agent.node_did().to_string();
    let agent_did = agent.default_agent_did().to_string();
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did.clone());
    runtime_status.initialize_startup(&agent_did).await.unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(8);
    let watcher_task = tokio::spawn(run_test_control_watcher(ControlWatcherInputs {
        node: node.clone(),
        backend_admission: agent.backend_admission.clone(),
        subscription: subscribe_control_changes(&node).unwrap(),
        node_did: node_did.clone(),
        tool_runtime: test_tool_runtime(&node, &node_did, &resolve_context),
        resolve_context,
        proposals_tx: proposal_tx,
        runtime_status,
        health_events_rx: mpsc::channel::<()>(1).1,
        shutdown: shutdown_rx,
    }));
    tokio::task::yield_now().await;

    write_tools_naming_plugin(
        &fixture.user_access,
        &node_did,
        &format!("{agent_did}:tools"),
    )
    .await;
    let absent = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("the Tools write naming a missing plugin must reach the reconcile owner")
        .expect("proposal channel");
    assert_eq!(
        absent.unavailable_agents[&agent_did].public_reason,
        AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
        "while the plugin is missing the agent's tools cannot build"
    );

    crate::plugin::store::write_record(plugin_home.path(), &installed_record()).unwrap();
    record_plugin_install(
        &fixture.agent_access,
        &fixture.node_binding,
        &node_did,
        plugin_home.path(),
    )
    .await;
    let installed = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("a pack install must wake the reconcile owner")
        .expect("proposal channel");
    assert_ne!(
        installed.configuration_fingerprint(),
        absent.configuration_fingerprint(),
        "the installed plugin identity must change the resolved fingerprint"
    );

    record_plugin_install(
        &fixture.agent_access,
        &fixture.node_binding,
        &node_did,
        plugin_home.path(),
    )
    .await;
    let reinstalled = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("the reinstall observation must reach the reconcile owner")
        .expect("proposal channel");
    assert_eq!(
        reinstalled.configuration_fingerprint(),
        installed.configuration_fingerprint(),
        "a same-digest reinstall must not produce a new fingerprint"
    );
    tokio::time::sleep(TEST_CONTROL_WATCHER_TIMING.settle_window).await;
    assert!(
        proposal_rx.try_recv().is_err(),
        "a settled same-fingerprint observation must not keep proposing"
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_keeps_an_agent_whose_tools_fail_to_build_demoted() {
    let fixture = plugin_runtime_fixture(
        "control-watcher-unbuildable-tools",
        "backend-unbuildable-tools",
        "http://127.0.0.1:8116/v1",
    )
    .await
    .unwrap();
    let node = fixture.node.clone();
    let identity = fixture.runtime.clone();
    let plugin_home = tempfile::tempdir().unwrap();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let node_did = agent.node_did().to_string();
    let agent_did = agent.default_agent_did().to_string();
    // As at startup: the Tools document already names a plugin that is not
    // installed, so the agent's tool surface resolves but cannot build.
    write_tools_naming_plugin(
        &fixture.user_access,
        &node_did,
        &format!("{agent_did}:tools"),
    )
    .await;
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did.clone());
    runtime_status.initialize_startup(&agent_did).await.unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(8);
    let (health_tx, health_rx) = mpsc::channel::<()>(1);
    let watcher_task = tokio::spawn(run_test_control_watcher(ControlWatcherInputs {
        node: node.clone(),
        backend_admission: agent.backend_admission.clone(),
        subscription: subscribe_control_changes(&node).unwrap(),
        node_did: node_did.clone(),
        tool_runtime: test_tool_runtime(&node, &node_did, &resolve_context),
        resolve_context,
        proposals_tx: proposal_tx,
        runtime_status,
        health_events_rx: health_rx,
        shutdown: shutdown_rx,
    }));

    // The watcher's first resolve after startup, woken by an unrelated event.
    health_tx.send(()).await.unwrap();
    let proposal = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("the health event must reach the reconcile owner")
        .expect("proposal channel");
    assert!(
        !proposal.agents.contains_key(&agent_did),
        "an agent whose tools fail to build must not come back runnable"
    );
    assert_eq!(
        proposal.unavailable_agents[&agent_did].public_reason,
        AgentReadinessUnavailableReason::ToolSurfaceUnavailable
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}
