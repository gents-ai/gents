use super::support::*;
use super::*;
use crate::agent::DocumentResolveContext;
use crate::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET;
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::runtime_status::{ReconcilePhase, RECONCILE_PHASE_EVENT_TARGET};
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

/// Collects the reconcile phases the watcher announced, in order.
///
/// A phase the watcher only passes through cannot be sampled from
/// `AgentRuntime`: the durable row keeps the latest phase, and one sample costs
/// a database round trip that can outlast the debounce interval itself.
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

/// Names the reconcile that never arrived instead of hanging the suite. The
/// proposal itself is the assertion; this bound only keeps a failure legible.
const PROPOSAL_TIMEOUT: Duration = Duration::from_secs(5);

async fn run_test_control_watcher(
    node: Arc<defra_node::EmbeddedNode>,
    subscription: events::DocumentChangeSubscription,
    agent_did: String,
    resolve_context: DocumentResolveContext,
    proposals_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    runtime_status: RuntimeStatusHandle,
    health_events_rx: mpsc::Receiver<()>,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    run_control_watcher_with_timing(
        node,
        subscription,
        agent_did,
        resolve_context,
        proposals_tx,
        runtime_status,
        health_events_rx,
        shutdown,
        TEST_CONTROL_WATCHER_TIMING,
    )
    .await
}

async fn update_agent_principal_enabled(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    enabled: bool,
) {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            update_AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                input: {{ enabled: {enabled} }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "update_AgentPrincipal failed: {:?}",
        response.errors
    );
}

async fn write_documents(
    node: &defra_node::EmbeddedNode,
    operation: &'static str,
    documents: Vec<(crate::Collection, serde_json::Value)>,
) {
    let documents = documents
        .into_iter()
        .map(
            |(collection, value)| crate::config_client::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect();
    let plan = crate::config_client::DesiredStateApplyPlan::new(documents).unwrap();
    crate::config_client::ConfigAccess::transact_local(node, None, operation, |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn control_watcher_publishes_reconciled_snapshot_after_relevant_update() {
    crate::test_support::enable_scoped_event_capture();
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-control",
        "http://127.0.0.1:8111/v1",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
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
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    runtime_status
        .initialize_startup(agent.default_behavior_id())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let reloads = RuntimeViewLoadCapture::default();
    let reload_count = Arc::clone(&reloads.count);
    let phases = ReconcilePhaseCapture::default();
    let announced_phases = phases.clone();
    let subscriber = Registry::default().with(reloads).with(phases);

    // Subscribe first, then publish before the watcher future is ever polled.
    // DefraDB subscriptions are live-only, so this deterministically guards
    // the startup ordering that prevents a post-Ready config update from
    // falling into the gap between readiness and watcher startup.
    let subscription = node.subscribe_document_changes();
    let context = serde_json::json!({
        "agent_did": agent.agent_did(),
        "context_id": format!("{}:context", agent.default_behavior_id()),
        "tools_id": format!("{}:tools", agent.default_behavior_id()),
        "system_prompt": "updated prompt"
    });
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::AgentContext,
            add: context.clone(),
            update: context,
        },
    ])
    .unwrap();
    crate::config_client::ConfigAccess::transact_local(
        node.as_ref(),
        None,
        "test.context.update",
        |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        },
    )
    .await
    .unwrap();

    let watcher_task = tokio::spawn(
        run_test_control_watcher(
            node.clone(),
            subscription,
            agent.agent_did().to_string(),
            resolve_context,
            proposal_tx,
            runtime_status.clone(),
            mpsc::channel::<()>(1).1,
            shutdown_rx,
        )
        .with_subscriber(subscriber),
    );

    let snapshot = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("an observed control update must reach the reconcile owner")
        .expect("reconciled snapshot");
    assert_eq!(
        snapshot
            .behaviors
            .get(agent.default_behavior_id())
            .expect("default behavior in snapshot")
            .system_prompt,
        "updated prompt"
    );
    assert_eq!(
        announced_phases.announced(),
        ["debouncing", "resolving"],
        "an observed control update debounces before it resolves"
    );
    let resolving = fetch_runtime_status(node.as_ref(), agent.agent_did()).await;
    assert_eq!(resolving.reconcile_phase, "resolving");

    runtime_status
        .set_reconcile_phase(ReconcilePhase::Idle)
        .await;
    let settled = fetch_runtime_status(node.as_ref(), agent.agent_did()).await;
    let loads_after_reconcile = reload_count.load(Ordering::Relaxed);
    assert!(
        loads_after_reconcile > 0,
        "runtime-view telemetry was captured"
    );
    tokio::time::sleep(TEST_CONTROL_WATCHER_TIMING.settle_retry * 5).await;
    tokio::task::yield_now().await;
    let retried = fetch_runtime_status(node.as_ref(), agent.agent_did()).await;
    assert_eq!(retried.reconcile_phase, "idle");
    assert_eq!(retried.updated_at, settled.updated_at);
    assert_eq!(
        reload_count.load(Ordering::Relaxed),
        loads_after_reconcile,
        "a visible reconcile must quiesce instead of polling the full runtime view"
    );

    // A new metadata-only write resolves to the same runtime fingerprint but
    // still needs a proposal so the reconciler can publish its normal no-op
    // completion. An unchanged settle retry above must remain suppressed.
    let context_id =
        crate::graphql::escape_graphql_string(&format!("{}:context", agent.default_behavior_id()));
    let mutation = format!(
        r#"mutation {{ update_AgentContext(
        filter: {{context_id: {{_eq: "{context_id}"}}}},
        input: {{display_name: "Retagged context"}}) {{context_id}} }}"#
    );
    crate::config_client::ConfigAccess::write_local(
        node.as_ref(),
        "test.context.metadata",
        &mutation,
    )
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

/// #2068: an operator's own committed write reconciles without waiting out the
/// replicated-arrival debounce, which Setup's save confirmation cannot outlast.
#[tokio::test]
async fn control_watcher_reconciles_a_local_write_without_the_replication_debounce() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-local"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-control",
        "http://127.0.0.1:8111/v1",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
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
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    runtime_status
        .initialize_startup(agent.default_behavior_id())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let subscription = node.subscribe_document_changes();
    let watcher_task = tokio::spawn(run_control_watcher_with_timing(
        node.clone(),
        subscription,
        agent.agent_did().to_string(),
        resolve_context,
        proposal_tx,
        runtime_status,
        mpsc::channel::<()>(1).1,
        shutdown_rx,
        ControlWatcherTiming {
            debounce: Duration::from_secs(3600),
            ..TEST_CONTROL_WATCHER_TIMING
        },
    ));

    write_documents(
        node.as_ref(),
        "test.context.local",
        vec![(
            crate::Collection::AgentContext,
            serde_json::json!({
                "agent_did": agent.agent_did(),
                "context_id": format!("{}:context", agent.default_behavior_id()),
                "tools_id": format!("{}:tools", agent.default_behavior_id()),
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
            .behaviors
            .get(agent.default_behavior_id())
            .expect("default behavior in snapshot")
            .system_prompt,
        "operator prompt"
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

/// #640: a measured-health flip must re-resolve the snapshot without any
/// document changing — demoting the behavior and marking the admission
/// config while the backend is measured unhealthy, and restoring both after
/// a successful probe flips the veto back.
#[tokio::test]
async fn control_watcher_demotes_and_recovers_behavior_on_measured_health_flip() {
    crate::test_support::enable_scoped_event_capture();
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-health"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-measured",
        "http://127.0.0.1:8113/v1",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
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
    let behavior_id = agent.default_behavior_id().to_string();
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    runtime_status
        .initialize_startup(agent.default_behavior_id())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let (health_tx, health_rx) = mpsc::channel::<()>(1);
    let phases = ReconcilePhaseCapture::default();
    let announced_phases = phases.clone();
    let subscriber = Registry::default().with(phases);

    let watcher_task = tokio::spawn(
        run_test_control_watcher(
            node.clone(),
            node.subscribe_document_changes(),
            agent.agent_did().to_string(),
            resolve_context,
            proposal_tx,
            runtime_status.clone(),
            health_rx,
            shutdown_rx,
        )
        .with_subscriber(subscriber),
    );

    tokio::task::yield_now().await;

    // The prober measured K consecutive failures: routing veto engages.
    backend_health
        .set_for_test(
            "backend-measured",
            crate::backend_health::BackendHealthState::Unhealthy,
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
        !snapshot.behaviors.contains_key(&behavior_id),
        "behavior on a measured-unhealthy backend must leave the active set"
    );
    let reason = snapshot
        .unavailable_behaviors
        .get(&behavior_id)
        .expect("unavailable reason for demoted behavior");
    assert_eq!(
        reason.public_reason,
        BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable
    );
    let config = snapshot
        .backend_admission_configs
        .get("backend-measured")
        .expect("admission config for measured backend");
    assert!(config.measured_unhealthy);

    // One successful probe re-promotes: routing resumes.
    backend_health
        .set_for_test(
            "backend-measured",
            crate::backend_health::BackendHealthState::Healthy,
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
        snapshot.behaviors.contains_key(&behavior_id),
        "behavior must return to the active set after recovery"
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
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-recover"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-control-recover",
        "http://127.0.0.1:8112/v1",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
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
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    runtime_status
        .initialize_startup(agent.default_behavior_id())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let reloads = RuntimeViewLoadCapture::default();
    let reload_count = Arc::clone(&reloads.count);
    let subscriber = Registry::default().with(reloads);

    let watcher_task = tokio::spawn(
        run_test_control_watcher(
            node.clone(),
            node.subscribe_document_changes(),
            agent.agent_did().to_string(),
            resolve_context,
            proposal_tx,
            runtime_status.clone(),
            mpsc::channel::<()>(1).1,
            shutdown_rx,
        )
        .with_subscriber(subscriber),
    );

    tokio::task::yield_now().await;
    update_agent_principal_enabled(node.as_ref(), agent.agent_did(), false).await;

    // A failed resolve is the only way back to idle with an error recorded, so
    // this is the reconcile's durable completion rather than a phase in flight.
    let failed_status =
        wait_for_runtime_reconcile_phase(node.as_ref(), agent.agent_did(), "idle").await;
    assert_eq!(failed_status.active_generation, 0);
    assert_eq!(failed_status.last_reconcile_result, "error");
    assert!(!failed_status.last_reconcile_error.is_empty());
    assert!(proposal_rx.try_recv().is_err());
    assert!(
        reload_count.load(Ordering::Relaxed) > 1,
        "a transient resolution failure must retry during the settle window"
    );

    update_agent_principal_enabled(node.as_ref(), agent.agent_did(), true).await;

    // The settle retry may already return the phase to idle after proposing
    // this fingerprint. Recovery is the queued proposal, not a transient phase.
    let snapshot = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("a re-enabled principal must reach the reconcile owner")
        .expect("recovered snapshot");
    assert_eq!(snapshot.default_behavior_id, agent.default_behavior_id());

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_resolves_context_tools_into_reconciled_tool_surface() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-tools"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-control-tools",
        "http://127.0.0.1:8113/v1",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
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
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    runtime_status
        .initialize_startup(agent.default_behavior_id())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);

    let watcher_task = tokio::spawn(run_test_control_watcher(
        node.clone(),
        node.subscribe_document_changes(),
        agent.agent_did().to_string(),
        resolve_context,
        proposal_tx,
        runtime_status.clone(),
        mpsc::channel::<()>(1).1,
        shutdown_rx,
    ));

    tokio::task::yield_now().await;

    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{}:tools", agent.default_behavior_id()),
        "agent_did": agent.agent_did(),
        "display_name": "Read tools",
        "host": {"files": {"mode": "ReadOnly"}}
    }))
    .unwrap();
    crate::config_client::write_tools_document(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &tools,
    )
    .await
    .unwrap();

    tokio::task::yield_now().await;
    tokio::time::sleep(TEST_CONTROL_WATCHER_TIMING.debounce + Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let snapshot = proposal_rx.recv().await.expect("reconciled snapshot");
    let tool_surface = snapshot
        .tool_surfaces
        .get(agent.default_behavior_id())
        .expect("default behavior tool surface");
    let tool_names = tool_surface.tool_names();
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"list_files".to_string()));
    assert!(!tool_names.contains(&"discover_tools".to_string()));

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_watcher_settles_when_an_unselected_behavior_is_permanently_invalid() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-settle"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-settle",
        "http://127.0.0.1:8114/v1",
    )
    .await;
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let selected_behavior_id = agent.default_behavior_id().to_string();

    let backend =
        crate::backend_registry::lookup_backend(node.as_ref(), &agent_did, "backend-settle")
            .await
            .unwrap()
            .expect("configured backend document");
    let advertised = |model_name: &str| crate::document_config::AdvertisedModel {
        model_name: model_name.into(),
        display_name: None,
        context_window: None,
        max_context_window: None,
        max_output_tokens: None,
        reasoning_efforts: None,
    };
    let catalog = |observed_at: &str, models: Vec<crate::document_config::AdvertisedModel>| {
        crate::document_config::BackendModelCatalog {
            agent_did: None,
            observed_at: observed_at.into(),
            models,
        }
    };
    // The spare profile is published while its model is advertised; a later
    // discovery drops that model, which leaves the profile permanently invalid.
    crate::backend_registry::record_model_catalog(
        node.as_ref(),
        &backend,
        catalog(
            "2026-01-01T00:00:00Z",
            vec![advertised("default"), advertised("retired-model")],
        ),
    )
    .await
    .unwrap();

    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent_did.clone());
    runtime_status
        .initialize_startup(&selected_behavior_id)
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);

    // Subscribe before writing: DefraDB subscriptions are live-only.
    let subscription = node.subscribe_document_changes();
    let spare_behavior_id = format!("{selected_behavior_id}:spare");
    let spare_profile_id = format!("{spare_behavior_id}:inference");
    write_documents(
        node.as_ref(),
        "test.settle.spare",
        vec![
            (
                crate::Collection::InferenceProfile,
                serde_json::json!({
                    "agent_did": agent_did,
                    "profile_id": spare_profile_id,
                    "backend_id": "backend-settle",
                    "model_name": "retired-model"
                }),
            ),
            (
                crate::Collection::AgentBehavior,
                serde_json::json!({
                    "agent_did": agent_did,
                    "behavior_id": spare_behavior_id,
                    "inference_profile_id": spare_profile_id
                }),
            ),
        ],
    )
    .await;
    crate::backend_registry::record_model_catalog(
        node.as_ref(),
        &backend,
        catalog("2026-01-02T00:00:00Z", vec![advertised("default")]),
    )
    .await
    .unwrap();

    let watcher_task = tokio::spawn(run_test_control_watcher(
        node.clone(),
        subscription,
        agent_did.clone(),
        resolve_context,
        proposal_tx,
        runtime_status.clone(),
        mpsc::channel::<()>(1).1,
        shutdown_rx,
    ));

    let snapshot = tokio::time::timeout(Duration::from_secs(5), proposal_rx.recv())
        .await
        .expect("a permanently invalid unselected behavior must not hold reconciliation")
        .expect("reconciled snapshot");
    assert!(snapshot.behaviors.contains_key(&selected_behavior_id));
    assert!(!snapshot.behaviors.contains_key(&spare_behavior_id));
    assert_eq!(
        snapshot
            .unavailable_behaviors
            .get(&spare_behavior_id)
            .expect("unavailable reason for the invalid behavior")
            .public_reason,
        BehaviorReadinessUnavailableReason::InferenceProfileInvalid
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();

    let view = crate::agent::document_view::load_document_runtime_view(node.as_ref(), &agent_did)
        .await
        .unwrap();
    assert!(
        !view.has_unresolved_behavior_references(),
        "permanently invalid inference selection is not a pending document: {:?}",
        view.pending_visibility_details()
    );
}

/// Writes (or refreshes) the PackInstallation record a plugin-store install
/// leaves, through the same owner the home install path records through, so
/// the watcher tests observe the document that install actually writes.
async fn write_pack_installation_record(node: &Arc<defra_node::EmbeddedNode>, agent_did: &str) {
    let identity = crate::pack::PackIdentity {
        coordinate: "fixture/bind_plugin_fixture".to_owned(),
        version: "0.1.0".to_owned(),
        digest: "sha256:pack-digest".to_owned(),
        plugins: Vec::new(),
        dependencies: Vec::new(),
    };
    crate::pack::record_plugin_store_install(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        agent_did,
        &identity,
    )
    .await
    .unwrap();
}

async fn write_tools_naming_plugin(
    node: &Arc<defra_node::EmbeddedNode>,
    agent_did: &str,
    tools_id: &str,
) {
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": tools_id,
        "agent_did": agent_did,
        "integrations": {"plugins": [{"plugin": "fixture/list_files"}]},
    }))
    .unwrap();
    crate::config_client::write_tools_document(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &tools,
    )
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

/// #2338: installing the plugin a Tools document names changes the resolved
/// surface, so the install record's write must wake the watcher and propose a
/// new fingerprint; a same-digest reinstall proposes the same fingerprint, so
/// the reconciler no-ops instead of churning generations.
#[tokio::test]
async fn control_watcher_proposes_a_new_fingerprint_when_the_named_plugin_installs() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-plugin-install"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-plugin-install",
        "http://127.0.0.1:8115/v1",
    )
    .await;
    let plugin_home = tempfile::tempdir().unwrap();
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let behavior_id = agent.default_behavior_id().to_string();
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent_did.clone());
    runtime_status
        .initialize_startup(&behavior_id)
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(8);
    let watcher_task = tokio::spawn(run_test_control_watcher(
        node.clone(),
        node.subscribe_document_changes(),
        agent_did.clone(),
        resolve_context,
        proposal_tx,
        runtime_status,
        mpsc::channel::<()>(1).1,
        shutdown_rx,
    ));
    tokio::task::yield_now().await;

    write_tools_naming_plugin(&node, &agent_did, &format!("{behavior_id}:tools")).await;
    let absent = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("the Tools write naming a missing plugin must reach the reconcile owner")
        .expect("proposal channel");
    let absent_surface = absent
        .tool_surfaces
        .get(&behavior_id)
        .expect("default behavior tool surface");
    assert_eq!(
        absent_surface.plugin_resolutions(),
        &[(
            crate::document_config::PluginToolRef {
                plugin: "fixture/list_files".to_string(),
                digest: None,
            },
            None
        )],
        "while the plugin is missing the surface records no identity"
    );

    // The install: the plugin store record, then the only document an
    // install writes.
    crate::plugin::store::write_record(plugin_home.path(), &installed_record()).unwrap();
    write_pack_installation_record(&node, &agent_did).await;
    let installed = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("a pack install must wake the reconcile owner")
        .expect("proposal channel");
    assert_ne!(
        installed.configuration_fingerprint(),
        absent.configuration_fingerprint(),
        "the installed plugin identity must change the resolved fingerprint"
    );

    // A same-digest reinstall rewrites the same record (a fresh
    // installed_at); the resolved identity is unchanged, so the proposal
    // repeats the fingerprint the reconciler already holds and nothing new
    // can apply.
    write_pack_installation_record(&node, &agent_did).await;
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

/// #2338 negative: while the named plugin stays missing, install-record
/// writes alone never change the resolved fingerprint, so they cannot churn
/// the active generation.
#[tokio::test]
async fn control_watcher_keeps_one_fingerprint_while_the_named_plugin_stays_missing() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-plugin-missing"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-plugin-missing",
        "http://127.0.0.1:8116/v1",
    )
    .await;
    let plugin_home = tempfile::tempdir().unwrap();
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let behavior_id = agent.default_behavior_id().to_string();
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent_did.clone());
    runtime_status
        .initialize_startup(&behavior_id)
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(8);
    let watcher_task = tokio::spawn(run_test_control_watcher(
        node.clone(),
        node.subscribe_document_changes(),
        agent_did.clone(),
        resolve_context,
        proposal_tx,
        runtime_status,
        mpsc::channel::<()>(1).1,
        shutdown_rx,
    ));
    tokio::task::yield_now().await;

    write_tools_naming_plugin(&node, &agent_did, &format!("{behavior_id}:tools")).await;
    let named = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
        .await
        .expect("the Tools write naming a missing plugin must reach the reconcile owner")
        .expect("proposal channel");
    let fingerprint = named.configuration_fingerprint();

    for _ in 0..2 {
        write_pack_installation_record(&node, &agent_did).await;
        let observed = tokio::time::timeout(PROPOSAL_TIMEOUT, proposal_rx.recv())
            .await
            .expect("each install-record observation must reach the reconcile owner")
            .expect("proposal channel");
        assert_eq!(
            observed.configuration_fingerprint(),
            fingerprint,
            "an install record for a plugin that is still missing must not change the fingerprint"
        );
    }
    tokio::time::sleep(TEST_CONTROL_WATCHER_TIMING.settle_window).await;
    assert!(
        proposal_rx.try_recv().is_err(),
        "no further proposals may arrive once the writes settle"
    );

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}
