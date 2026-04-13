use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use defra_node::EventName;
use rig::client::CompletionClient;
use tokio::sync::{mpsc, watch, Mutex, Notify};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::daemon::BehaviorDaemon;
use super::reconcile::GenerationSupervisor;
use super::{DefraAgent, ProcessLifecycleState};
use crate::backend_provider::{build_completion_client, discover_models};
use crate::backend_registry::BackendTracker;
use crate::health_checker::{spawn_health_checker, ServiceHealthMap};
use crate::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use crate::prompt::LayeredPromptBuilder;
use crate::retry::RetryPolicy;
use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ResolvedRuntimeSnapshot};
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};
use crate::streaming::{DefraStreamWriter, StreamStatus, StreamWriter};
use crate::tool_surface::{ToolRuntimeContext, ToolSurface};
use crate::watcher::{AgentRequest, DefraWatcher, Watcher};

#[derive(Clone)]
struct RuntimeContext {
    node: Arc<defra_node::EmbeddedNode>,
    tool_runtime: ToolRuntimeContext,
    backend_tracker: Arc<BackendTracker>,
    retry_policy: RetryPolicy,
    hook_failure_policy: crate::hook::FailurePolicy,
    startup_barrier: Arc<StartupBarrier>,
}

struct BehaviorResolution {
    behavior_id: String,
    rejection_reason: Option<String>,
}

pub(super) struct StartupBarrier {
    pending_behaviors: Mutex<HashSet<String>>,
    notify: Notify,
}

enum BackgroundTaskResult {
    Router(Result<()>),
    RouterObserver(Result<()>),
    Reconcile(Result<()>),
    Control(Result<()>),
}

const STARTUP_BACKEND_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

impl StartupBarrier {
    fn new(behaviors: &[Arc<crate::config::BehaviorConfig>]) -> Self {
        Self {
            pending_behaviors: Mutex::new(
                behaviors
                    .iter()
                    .map(|behavior| behavior.name.clone())
                    .collect::<HashSet<_>>(),
            ),
            notify: Notify::new(),
        }
    }

    pub(super) async fn mark_behavior_ready(&self, behavior_id: &str) {
        let mut pending = self.pending_behaviors.lock().await;
        let removed = pending.remove(behavior_id);
        let is_empty = pending.is_empty();
        drop(pending);

        if removed && is_empty {
            self.notify.notify_waiters();
        }
    }

    pub(super) async fn wait_ready(&self) {
        loop {
            if self.pending_behaviors.lock().await.is_empty() {
                return;
            }
            self.notify.notified().await;
        }
    }
}

impl RuntimeContext {
    async fn run_behavior(
        &self,
        behavior: Arc<crate::config::BehaviorConfig>,
        tool_surface: Arc<ToolSurface>,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let tool_names = tool_surface.tool_names();
        let api_key = behavior.completion_client_api_key()?;
        let prompt_builder = LayeredPromptBuilder::new(behavior.as_ref(), tool_surface.as_ref());
        let preamble = prompt_builder.preamble().to_string();
        let tools = tool_surface.build_tools(&self.tool_runtime)?;
        behavior.ensure_runtime_compatibility(tool_surface.as_ref())?;
        let openai_client = build_completion_client(
            behavior.backend_provider_kind,
            &behavior.backend_endpoint,
            &api_key,
        )?;
        tracing::info!(
            behavior_id = %behavior.name,
            did = %behavior.did(),
            model = %behavior.model_name,
            tools = ?tool_names,
            "building behavior runtime"
        );

        let agent = openai_client
            .agent(&behavior.model_name)
            .preamble(&preamble)
            .default_max_turns(behavior.max_turns)
            .tools(tools)
            .build();
        let mut daemon = BehaviorDaemon::new(
            self.node.clone(),
            behavior,
            agent,
            prompt_builder,
            self.backend_tracker.clone(),
            self.retry_policy.clone(),
            self.hook_failure_policy,
            self.startup_barrier.clone(),
        );
        daemon.run(request_rx, shutdown).await
    }
}

pub(super) async fn run_agent(
    agent: DefraAgent,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let cancel = CancellationToken::new();
    let runtime_status = RuntimeStatusHandle::new(agent.node.clone(), agent.agent_did.clone());
    runtime_status
        .set_process_state(ProcessLifecycleState::Recovering)
        .await;
    runtime_status
        .set_reconcile_phase(ReconcilePhase::Resolving)
        .await;
    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::Recovering);
    }
    let health_map = ServiceHealthMap::new();
    let tool_runtime = ToolRuntimeContext::new(
        agent.node.clone(),
        agent.mcp_pool.clone(),
        health_map.clone(),
        agent.local_hostname.clone(),
        agent.local_subnet.clone(),
    );
    let resolved_snapshot = match resolve_startup_snapshot(&agent).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            runtime_status.publish_error(&format!("{error:#}")).await;
            return Err(error);
        }
    };
    if let Err(error) = validate_startup_snapshot(&agent, &tool_runtime, &resolved_snapshot).await {
        runtime_status.publish_error(&format!("{error:#}")).await;
        return Err(error);
    }
    let _health_checker = spawn_health_checker(
        agent.node.clone(),
        agent.mcp_pool.clone(),
        health_map.clone(),
        agent.local_hostname.clone(),
        agent.local_subnet.clone(),
        cancel.child_token(),
    );

    log_recovery(
        agent.node.as_ref(),
        &agent.agent_did,
        &agent.default_behavior_id,
    )
    .await;
    for (behavior_id, reason) in &agent.unavailable_behaviors {
        tracing::warn!(behavior_id = %behavior_id, reason = %reason, "behavior unavailable at startup");
    }

    let startup_barrier = Arc::new(StartupBarrier::new(
        &resolved_snapshot
            .behaviors
            .values()
            .cloned()
            .collect::<Vec<_>>(),
    ));
    let backend_tracker = Arc::new(BackendTracker::new());
    let runtime = RuntimeContext {
        node: agent.node.clone(),
        tool_runtime,
        backend_tracker: backend_tracker.clone(),
        retry_policy: agent.retry_policy.clone(),
        hook_failure_policy: agent.hook_failure_policy,
        startup_barrier: startup_barrier.clone(),
    };
    let scheduler_tool_runtime = runtime.tool_runtime.clone();
    let runtime_for_runner = runtime.clone();
    let generation_supervisor = GenerationSupervisor::bootstrap(
        resolved_snapshot,
        agent.retry_policy.clone(),
        move |behavior, tool_surface, request_rx, shutdown| {
            let runtime = runtime_for_runner.clone();
            async move {
                runtime
                    .run_behavior(behavior, tool_surface, request_rx, shutdown)
                    .await
            }
        },
        runtime_status.clone(),
        shutdown.clone(),
    )?;
    let initial_active_snapshot = generation_supervisor.current_snapshot();
    runtime_status
        .publish_startup_snapshot(initial_active_snapshot.as_ref())
        .await;
    let (active_snapshot_tx, active_snapshot_rx) = watch::channel(initial_active_snapshot.clone());
    let (reconcile_tx, reconcile_rx) = mpsc::channel(8);
    let _reconcile_tx_guard = reconcile_tx.clone();

    let scheduler = crate::scheduler::Scheduler::new(
        agent.node.clone(),
        active_snapshot_rx.clone(),
        scheduler_tool_runtime,
        backend_tracker.clone(),
    );

    let scheduler_cancel = cancel.child_token();
    let scheduler_startup_barrier = startup_barrier.clone();
    let scheduler_handle = tokio::spawn(async move {
        let mut scheduler = scheduler;
        tokio::select! {
            _ = scheduler_cancel.cancelled() => return,
            _ = scheduler_startup_barrier.wait_ready() => {}
        }
        if let Err(error) = scheduler.run(scheduler_cancel).await {
            tracing::error!(error = %error, "scheduler exited with error");
        }
    });

    let ready_cancel = cancel.child_token();
    let ready_startup_barrier = startup_barrier.clone();
    let ready_observer = agent.process_state_observer.clone();
    let ready_runtime_status = runtime_status.clone();
    let ready_behavior_count = initial_active_snapshot.behaviors.len();
    let ready_unavailable_count = initial_active_snapshot.unavailable_behaviors.len();
    let readiness_handle = tokio::spawn(async move {
        tokio::select! {
            _ = ready_cancel.cancelled() => return,
            _ = ready_startup_barrier.wait_ready() => {}
        }
        ready_runtime_status
            .set_process_state(ProcessLifecycleState::Ready)
            .await;
        if let Some(observer) = &ready_observer {
            observer.on_process_state_change(ProcessLifecycleState::Ready);
        }
        tracing::info!(
            runnable_behaviors = ready_behavior_count,
            unavailable_behaviors = ready_unavailable_count,
            "defra-agent ready"
        );
    });

    let mut background_tasks = JoinSet::new();

    let router_node = agent.node.clone();
    let router_agent_did = agent.agent_did.clone();
    let router_active_snapshot_rx = active_snapshot_rx.clone();
    let router_shutdown = shutdown.clone();
    background_tasks.spawn(async move {
        BackgroundTaskResult::Router(
            run_router(
                router_node,
                router_agent_did,
                router_active_snapshot_rx,
                router_shutdown,
            )
            .await,
        )
    });

    let router_observer_active_snapshot_rx = active_snapshot_rx.clone();
    let router_observer_runtime_status = runtime_status.clone();
    let router_observer_shutdown = shutdown.clone();
    background_tasks.spawn(async move {
        BackgroundTaskResult::RouterObserver(
            run_router_generation_observer(
                router_observer_active_snapshot_rx,
                router_observer_runtime_status,
                router_observer_shutdown,
            )
            .await,
        )
    });

    let reconcile_active_snapshot_tx = active_snapshot_tx.clone();
    let reconcile_shutdown = shutdown.clone();
    background_tasks.spawn(async move {
        BackgroundTaskResult::Reconcile(
            generation_supervisor
                .run(
                    reconcile_active_snapshot_tx,
                    reconcile_rx,
                    reconcile_shutdown,
                )
                .await,
        )
    });

    if agent.document_runtime_context().is_some() {
        let control_node = agent.node.clone();
        let control_agent_did = agent.agent_did.clone();
        let control_context = agent
            .document_runtime_context()
            .cloned()
            .expect("checked document runtime context");
        let control_tx = reconcile_tx.clone();
        let control_runtime_status = runtime_status.clone();
        let control_shutdown = shutdown.clone();
        background_tasks.spawn(async move {
            BackgroundTaskResult::Control(
                run_control_watcher(
                    control_node,
                    control_agent_did,
                    control_context,
                    control_tx,
                    control_runtime_status,
                    control_shutdown,
                )
                .await,
            )
        });
    }

    let (result, shutdown_requested) = tokio::select! {
        _ = shutdown.changed() => (Ok(()), true),
        Some(joined) = background_tasks.join_next() => match joined {
            Ok(BackgroundTaskResult::Router(result)) => (result, false),
            Ok(BackgroundTaskResult::RouterObserver(result)) => (result, false),
            Ok(BackgroundTaskResult::Reconcile(result)) => (result, false),
            Ok(BackgroundTaskResult::Control(result)) => (result, false),
            Err(error) => (Err(anyhow!("background task join failed: {error}")), false),
        },
        else => (Ok(()), false),
    };

    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::ShuttingDown);
    }
    runtime_status
        .set_process_state(ProcessLifecycleState::ShuttingDown)
        .await;

    cancel.cancel();
    if !shutdown_requested {
        background_tasks.abort_all();
    }
    while let Some(joined) = background_tasks.join_next().await {
        if let Err(error) = joined {
            if !error.is_cancelled() {
                tracing::error!(error = %error, "background task exited during shutdown");
            }
        }
    }

    let _ = readiness_handle.await;
    let _ = scheduler_handle.await;

    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::Shutdown);
    }
    runtime_status
        .set_process_state(ProcessLifecycleState::Shutdown)
        .await;

    result
}

async fn log_recovery(node: &defra_node::EmbeddedNode, agent_did: &str, default_behavior_id: &str) {
    match RequestLifecycle::recover_all(node, agent_did).await {
        Ok(report) => {
            if report.requests_recovered > 0 {
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.requests_recovered,
                    "recovered stuck requests"
                );
            }
            if report.responses_recovered > 0 {
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.responses_recovered,
                    "recovered stuck responses"
                );
            }
            if report.conversations_recovered > 0 {
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.conversations_recovered,
                    "recovered stuck conversations"
                );
            }
            if report.requests_recovered == 0
                && report.responses_recovered == 0
                && report.conversations_recovered == 0
            {
                tracing::debug!(
                    agent_did = %agent_did,
                    default_behavior_id = %default_behavior_id,
                    "startup recovery found no stuck documents"
                );
            }
        }
        Err(error) => {
            tracing::warn!(agent_did = %agent_did, error = %error, "startup recovery failed");
        }
    }
}

async fn validate_startup_snapshot(
    agent: &DefraAgent,
    tool_runtime: &ToolRuntimeContext,
    snapshot: &ResolvedRuntimeSnapshot,
) -> Result<()> {
    if snapshot.behaviors.is_empty() {
        let mut unavailable = snapshot
            .unavailable_behaviors
            .iter()
            .map(|(behavior_id, reason)| format!("{behavior_id}: {reason}"))
            .collect::<Vec<_>>();
        unavailable.sort();
        if unavailable.is_empty() {
            anyhow::bail!(
                "agent {} has no runnable behaviors at startup",
                agent.agent_did()
            );
        }
        anyhow::bail!(
            "agent {} has no runnable behaviors at startup ({})",
            agent.agent_did(),
            unavailable.join("; ")
        );
    }

    let client = reqwest::Client::builder()
        .timeout(STARTUP_BACKEND_PROBE_TIMEOUT)
        .build()
        .context("building startup readiness probe client")?;
    let mut behavior_ids = snapshot.behaviors.keys().cloned().collect::<Vec<_>>();
    behavior_ids.sort();

    for behavior_id in behavior_ids {
        let behavior = snapshot
            .behaviors
            .get(&behavior_id)
            .expect("behavior id came from snapshot.behaviors");
        let tool_surface = snapshot
            .tool_surfaces
            .get(&behavior_id)
            .ok_or_else(|| anyhow!("missing tool surface for behavior {behavior_id}"))?;
        behavior.ensure_runtime_compatibility(tool_surface.as_ref())?;
        tool_surface
            .build_tools(tool_runtime)
            .with_context(|| format!("building startup tool surface for behavior {behavior_id}"))?;
        let api_key = behavior.resolve_backend_api_key()?;
        probe_behavior_backend(&client, api_key.as_deref(), behavior.as_ref())
            .await
            .with_context(|| format!("validating startup backend for behavior {behavior_id}"))?;
    }

    Ok(())
}

async fn probe_behavior_backend(
    client: &reqwest::Client,
    api_key: Option<&str>,
    behavior: &crate::config::BehaviorConfig,
) -> Result<()> {
    let discovered_models = discover_models(
        client,
        behavior.backend_provider_kind,
        &behavior.backend_endpoint,
        api_key,
    )
    .await?;
    if !discovered_models
        .iter()
        .any(|model| model == &behavior.model_name)
    {
        anyhow::bail!(
            "startup readiness probe for behavior {} did not advertise model {} on backend {} ({})",
            behavior.name,
            behavior.model_name,
            behavior.backend_id.as_deref().unwrap_or("<unbound>"),
            behavior.backend_provider_kind
        );
    }

    Ok(())
}

async fn run_router(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: String,
    active_snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let watcher = DefraWatcher::new(node.clone(), &agent_did);
    run_router_with_watcher(node, agent_did, watcher, active_snapshot_rx, shutdown).await
}

async fn run_router_with_watcher<W>(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: String,
    mut watcher: W,
    mut active_snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()>
where
    W: Watcher,
{
    let mut active_snapshot = active_snapshot_rx.borrow().clone();

    loop {
        let Some(request) = wait_for_next_request_with_latest_snapshot(
            &agent_did,
            &mut watcher,
            &mut active_snapshot,
            &mut active_snapshot_rx,
            &mut shutdown,
        )
        .await?
        else {
            return Ok(());
        };

        let resolution = resolve_behavior_for_request(
            node.as_ref(),
            &request,
            active_snapshot.default_behavior_id.as_str(),
        )
        .await?;
        if let Some(reason) = resolution.rejection_reason.as_deref() {
            tracing::warn!(
                request_id = %request.request_id,
                session_id = %request.session_id,
                behavior_id = %resolution.behavior_id,
                reason = %reason,
                "rejecting request before dispatch"
            );
            fail_routed_request(
                node.clone(),
                agent_did.as_str(),
                request,
                resolution.behavior_id.as_str(),
                reason,
            )
            .await?;
            continue;
        }

        match active_snapshot.dispatchers.get(&resolution.behavior_id) {
            Some(dispatcher) => {
                tracing::info!(
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %resolution.behavior_id,
                    "dispatching request to behavior executor"
                );
                dispatcher.send(request).await.map_err(|_| {
                    anyhow!(
                        "executor queue for behavior {} closed unexpectedly",
                        resolution.behavior_id
                    )
                })?;
            }
            None => {
                let error_message = active_snapshot
                    .unavailable_reason(&resolution.behavior_id)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| {
                        format!(
                            "behavior {} is not loaded for principal {}",
                            resolution.behavior_id, agent_did
                        )
                    });
                tracing::warn!(
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %resolution.behavior_id,
                    reason = %error_message,
                    "behavior unavailable for request"
                );
                fail_routed_request(
                    node.clone(),
                    agent_did.as_str(),
                    request,
                    resolution.behavior_id.as_str(),
                    error_message.as_str(),
                )
                .await?;
            }
        }
    }
}

async fn wait_for_next_request_with_latest_snapshot<W>(
    agent_did: &str,
    watcher: &mut W,
    active_snapshot: &mut Arc<ActiveRuntimeSnapshot>,
    active_snapshot_rx: &mut watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Option<AgentRequest>>
where
    W: Watcher,
{
    loop {
        *active_snapshot = active_snapshot_rx.borrow_and_update().clone();
        let request = tokio::select! {
            biased;

            _ = shutdown.changed() => return Ok(None),
            changed = active_snapshot_rx.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
                *active_snapshot = active_snapshot_rx.borrow_and_update().clone();
                continue;
            }
            req = watcher.next_request() => {
                match req {
                    Some(Ok(req)) => req,
                    Some(Err(error)) => {
                        tracing::error!(agent_did = %agent_did, error = %error, "watcher error, retrying");
                        continue;
                    }
                    None => return Ok(None),
                }
            }
        };
        *active_snapshot = active_snapshot_rx.borrow_and_update().clone();
        return Ok(Some(request));
    }
}

async fn run_router_generation_observer(
    mut active_snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    runtime_status: RuntimeStatusHandle,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut observed_generation = 0u64;

    loop {
        let active_snapshot = active_snapshot_rx.borrow().clone();
        if observed_generation != active_snapshot.generation {
            runtime_status
                .publish_router_generation(active_snapshot.generation)
                .await;
            observed_generation = active_snapshot.generation;
        }

        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            changed = active_snapshot_rx.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

async fn resolve_behavior_for_request(
    node: &defra_node::EmbeddedNode,
    request: &AgentRequest,
    default_behavior_id: &str,
) -> Result<BehaviorResolution> {
    let requested_behavior_id =
        normalize_optional_string(request.behavior_id.as_deref()).map(ToOwned::to_owned);
    let session_behavior_id =
        crate::session::load_session_behavior_id(node, &request.session_id).await?;
    let behavior_id = requested_behavior_id
        .clone()
        .or_else(|| session_behavior_id.clone())
        .unwrap_or_else(|| default_behavior_id.to_string());

    let rejection_reason = match (
        session_behavior_id.as_deref(),
        requested_behavior_id.as_deref(),
    ) {
        (Some(existing), Some(requested)) if existing != requested => Some(format!(
            "session {} is pinned to behavior {} and cannot switch to {}",
            request.session_id, existing, requested
        )),
        _ => None,
    };

    Ok(BehaviorResolution {
        behavior_id,
        rejection_reason,
    })
}

async fn fail_routed_request(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: &str,
    request: AgentRequest,
    behavior_id: &str,
    error_message: &str,
) -> Result<()> {
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        behavior_id,
        agent_did,
        request.clone(),
        Duration::from_secs(30).as_secs(),
        ExecutionOrigin::Interactive,
        "",
    );

    match lifecycle.claim_with_identity().await {
        Ok(ClaimOutcome::Claimed) => {}
        Ok(ClaimOutcome::Superseded) => return Ok(()),
        Err(error) => {
            tracing::warn!(
                request_id = %request.request_id,
                session_id = %request.session_id,
                behavior_id = %behavior_id,
                error = %error,
                "failed to claim rejected request"
            );
            return Ok(());
        }
    }

    let _ = lifecycle.record_failure_reason(error_message).await;
    let _ = lifecycle.fail().await;
    if lifecycle.response_exists().await.unwrap_or(false) {
        return Ok(());
    }

    let stream_writer = DefraStreamWriter::new(node, agent_did, Duration::from_millis(0));
    let doc_id = stream_writer
        .begin(&request.session_id, &request.request_id, behavior_id)
        .await?;
    stream_writer
        .set_error_message(&doc_id, error_message)
        .await?;
    let _ = stream_writer
        .write_tokens(&doc_id, &format!("Error: {error_message}"))
        .await?;
    stream_writer.finalize(&doc_id, StreamStatus::Error).await?;
    Ok(())
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(super) fn default_hostname() -> String {
    hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

const CONTROL_RECONCILE_DEBOUNCE: Duration = Duration::from_secs(5);
const CONTROL_RECONCILE_SETTLE_RETRY: Duration = Duration::from_millis(500);
const CONTROL_RECONCILE_SETTLE_WINDOW: Duration = Duration::from_secs(5);
const CONTROL_WATCHER_IDLE_SLEEP: Duration = Duration::from_secs(60 * 60 * 24 * 365);

async fn run_control_watcher(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: String,
    resolve_context: super::DocumentResolveContext,
    proposals_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    runtime_status: RuntimeStatusHandle,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut document_view =
        super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await?;
    let mut subscription = node.subscribe(&[EventName::Update]);
    let sleep = tokio::time::sleep(CONTROL_WATCHER_IDLE_SLEEP);
    tokio::pin!(sleep);
    let mut dirty = false;
    let mut pending_visibility = false;
    let mut settle_deadline = None;
    let mut last_proposed_fingerprint = None::<String>;

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = &mut sleep, if dirty => {
                if pending_visibility {
                    match super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await {
                        Ok(reloaded) => {
                            document_view = reloaded;
                            pending_visibility = document_view.has_unresolved_behavior_references();
                        }
                        Err(error) => {
                            tracing::error!(
                                agent_did = %agent_did,
                                error = %error,
                                "runtime control watcher failed to refresh document view for pending visibility"
                            );
                            runtime_status.publish_error(&format!("{error:#}")).await;
                            if settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline) {
                                dirty = true;
                                sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_RETRY);
                            } else {
                                dirty = false;
                                settle_deadline = None;
                                sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_WATCHER_IDLE_SLEEP);
                            }
                            continue;
                        }
                    }
                }
                if pending_visibility
                    && settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline)
                {
                    dirty = true;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_RETRY);
                    continue;
                }
                runtime_status
                    .set_reconcile_phase(ReconcilePhase::Resolving)
                    .await;
                match super::document_view::resolve_document_runtime_snapshot_from_view(
                    node.as_ref(),
                    &resolve_context,
                    &document_view,
                )
                .await
                {
                    Ok(snapshot) => {
                        let fingerprint = snapshot.configuration_fingerprint();
                        if last_proposed_fingerprint.as_deref() != Some(fingerprint.as_str()) {
                            if proposals_tx.send(snapshot).await.is_err() {
                                return Ok(());
                            }
                            last_proposed_fingerprint = Some(fingerprint);
                        }
                    }
                    Err(error) => {
                        tracing::error!(
                            agent_did = %agent_did,
                            error = %error,
                            "runtime reconcile resolve failed; keeping previous active generation"
                        );
                        runtime_status.publish_error(&format!("{error:#}")).await;
                    }
                }
                if settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline) {
                    dirty = true;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_RETRY);
                } else {
                    dirty = false;
                    settle_deadline = None;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_WATCHER_IDLE_SLEEP);
                }
            }
            message = subscription.recv() => {
                let Some(message) = message else {
                    return Ok(());
                };

                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(
                        agent_did = %agent_did,
                        dropped = dropped,
                        "runtime control watcher dropped events, forcing full reconcile"
                    );
                    match super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await {
                        Ok(reloaded) => {
                            document_view = reloaded;
                            pending_visibility = document_view.has_unresolved_behavior_references();
                        }
                        Err(error) => {
                            tracing::error!(
                                agent_did = %agent_did,
                                error = %error,
                                "runtime control watcher failed to resync document view after dropped events"
                            );
                            runtime_status.publish_error(&format!("{error:#}")).await;
                            continue;
                        }
                    }
                    dirty = true;
                    settle_deadline =
                        Some(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_WINDOW);
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Debouncing)
                        .await;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_DEBOUNCE);
                    continue;
                }

                let Some(update) = message.as_update() else {
                    continue;
                };
                match super::document_view::apply_control_update(
                    node.as_ref(),
                    &agent_did,
                    update.collection_id.as_str(),
                    &update.doc_id,
                    &mut document_view,
                )
                .await
                {
                    Ok(super::document_view::ControlUpdateOutcome::Irrelevant) => continue,
                    Ok(super::document_view::ControlUpdateOutcome::Applied) => {}
                    Ok(super::document_view::ControlUpdateOutcome::PendingVisibility) => {
                        pending_visibility = true;
                    }
                    Err(error) => {
                        tracing::error!(
                            agent_did = %agent_did,
                            collection_id = %update.collection_id,
                            doc_id = %update.doc_id,
                            error = %error,
                            "runtime control update apply failed; forcing full resync"
                        );
                        match super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await {
                            Ok(reloaded) => {
                                document_view = reloaded;
                                pending_visibility = document_view.has_unresolved_behavior_references();
                            }
                            Err(resync_error) => {
                                tracing::error!(
                                    agent_did = %agent_did,
                                    error = %resync_error,
                                    "runtime control watcher failed to resync document view after update error"
                                );
                                runtime_status
                                    .publish_error(&format!("{resync_error:#}"))
                                    .await;
                                continue;
                            }
                        }
                    }
                }

                tracing::info!(
                    agent_did = %agent_did,
                    doc_id = %update.doc_id,
                    collection_id = %update.collection_id,
                    is_relay = update.is_relay,
                    "runtime control update detected"
                );
                dirty = true;
                settle_deadline =
                    Some(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_WINDOW);
                runtime_status
                    .set_reconcile_phase(ReconcilePhase::Debouncing)
                    .await;
                sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_DEBOUNCE);
            }
        }
    }
}

async fn resolve_tool_surfaces(
    node: &defra_node::EmbeddedNode,
    behaviors: &[Arc<crate::config::BehaviorConfig>],
) -> Result<HashMap<String, Arc<ToolSurface>>> {
    let mut tool_surfaces = HashMap::with_capacity(behaviors.len());
    for behavior in behaviors {
        let tool_surface = behavior.tools.resolve(node).await?;
        tool_surfaces.insert(behavior.name.clone(), Arc::new(tool_surface));
    }
    Ok(tool_surfaces)
}

async fn resolve_startup_snapshot(agent: &DefraAgent) -> Result<ResolvedRuntimeSnapshot> {
    match agent.document_runtime_context() {
        Some(resolve_context) => {
            resolve_document_snapshot_with_tools(agent.node.as_ref(), resolve_context).await
        }
        None => {
            let tool_surfaces =
                resolve_tool_surfaces(agent.node.as_ref(), &agent.behaviors).await?;
            Ok(ResolvedRuntimeSnapshot::from_parts(
                agent.default_behavior_id.clone(),
                agent.behaviors.clone(),
                tool_surfaces,
                agent.unavailable_behaviors.clone(),
            ))
        }
    }
}

async fn resolve_document_snapshot_with_tools(
    node: &defra_node::EmbeddedNode,
    resolve_context: &super::DocumentResolveContext,
) -> Result<ResolvedRuntimeSnapshot> {
    super::resolve_document_runtime_snapshot(node, resolve_context).await
}

#[cfg(test)]
mod tests;
