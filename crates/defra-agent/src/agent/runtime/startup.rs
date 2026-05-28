// Soft-cap justified: agent startup sequence with strong ordering constraints
// between health checking, snapshot resolution, slot bootstrap, and recovery.
// Each phase depends on the previous; splitting into submodules would require
// threading many intermediate values across module boundaries for no gain.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::context::{RuntimeContext, StartupBarrier};
use crate::admission::{AdmissionRegistry, BackendAdmissionConfig, InferenceCall};
use crate::agent::reconcile::GenerationSupervisor;
use crate::agent::{DefraAgent, DocumentResolveContext, ProcessLifecycleState};
use crate::backend_registry;
use crate::health_checker::{spawn_health_checker, ServiceHealthMap};
use crate::lifecycle::RequestLifecycle;
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};
use crate::tool_call_lifecycle::ToolCallLifecycle;
use crate::tool_surface::{ToolRuntimeContext, ToolSurface};

enum BackgroundTaskResult {
    Router(Result<()>),
    RouterObserver(Result<()>),
    Reconcile(Result<()>),
    Control(Result<()>),
    SubagentCompletion(Result<()>),
    CrossDeploymentCancelMirror(Result<()>),
}

pub(in crate::agent) async fn run_agent(
    agent: DefraAgent,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let cancel = CancellationToken::new();
    let runtime_status =
        RuntimeStatusHandle::new(agent.node.clone(), agent.agent_did().to_string());
    runtime_status
        .set_process_state(ProcessLifecycleState::Recovering)
        .await;
    runtime_status
        .set_reconcile_phase(ReconcilePhase::Resolving)
        .await;
    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::Recovering);
    }
    if let Err(error) = crate::migration::ensure_peer_pairing_desired_migrations(agent.node.clone())
        .await
        .context("ensure PeerPairingDesired migrations")
    {
        runtime_status.publish_error(&format!("{error:#}")).await;
        return Err(error);
    }
    let health_map = ServiceHealthMap::new();
    let tool_runtime = ToolRuntimeContext::new(
        agent.node.clone(),
        agent.mcp_pool.clone(),
        health_map.clone(),
        agent.local_hostname.clone(),
        agent.local_subnet.clone(),
    );
    // Promote reachable enabled backends to healthy before resolving which
    // behaviors are runnable. A fresh store's backends start `probe_status=
    // unknown`, and nothing else promotes them, so without this the runtime
    // comes up with zero runnable behaviors until an operator manually runs
    // `config backend set --probe-status healthy`.
    backend_registry::probe_and_promote_enabled_backends(agent.node.as_ref()).await;

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
        agent.health_checker_options.clone(),
        agent.agent_did().to_string(),
    );

    log_recovery(
        agent.node.as_ref(),
        agent.agent_did(),
        agent.default_behavior_id(),
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
    let admission_registry = AdmissionRegistry::new(agent.node.clone());
    let runtime = RuntimeContext {
        node: agent.node.clone(),
        tool_runtime,
        admission_registry: admission_registry.clone(),
        retry_policy: agent.retry_policy.clone(),
        hook_failure_policy: agent.hook_failure_policy,
        background_execution_registry: agent.background_execution_registry.clone(),
        startup_barrier: startup_barrier.clone(),
    };
    let runtime_for_runner = runtime.clone();
    let generation_supervisor = GenerationSupervisor::bootstrap(
        resolved_snapshot,
        admission_registry.clone(),
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

    // The legacy scheduler module has been replaced by the event-driven
    // `TriggerEngine`. Construct a `ScheduleSource` and an `EventSource`
    // backed by the active runtime snapshot, plus a `ProductionMaterializer`
    // that writes `AgentRequest` documents with `caused_by_trigger_{id,kind}`
    // lineage via the lifecycle module. The materializer enqueues Pending
    // requests; the normal watcher/router path claims and executes them.
    let trigger_engine_node = agent.node.clone();
    let trigger_engine_schedule_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_event_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_subagent_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_engine_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_materializer_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_cancel = cancel.child_token();
    let trigger_engine_startup_barrier = startup_barrier.clone();
    // Construct the `ManualSource` up-front so the `ManualTriggerHandle` can
    // be published to in-process callers (via the `OnceCell` on
    // `DefraAgent`) before `run()` awaits shutdown. Deferring construction
    // into the spawned task would race the callers that cloned `DefraAgent`
    // and are polling for the handle.
    let (manual_source, manual_trigger_handle) =
        crate::trigger_engine::manual_source::ManualSource::new(trigger_engine_cancel.clone());
    // Publish the handle; `set` returns `Err` only if another path already
    // populated the cell, which is not expected here but is harmless to
    // ignore — the handle is `Clone` and all copies route to the same
    // channel sender.
    let _ = agent.manual_trigger_handle.set(manual_trigger_handle);
    let trigger_engine_handle = tokio::spawn(async move {
        tokio::select! {
            _ = trigger_engine_cancel.cancelled() => return,
            _ = trigger_engine_startup_barrier.wait_ready() => {}
        }
        let materializer: Arc<dyn crate::trigger_engine::MaterializerHandle> = Arc::new(
            crate::trigger_engine::production_materializer::ProductionMaterializer::new(
                trigger_engine_node.clone(),
                trigger_engine_materializer_snapshot_rx,
            ),
        );
        let schedule_source: Box<dyn crate::trigger_engine::TriggerSource> =
            Box::new(crate::trigger_engine::schedule_source::ScheduleSource::new(
                trigger_engine_schedule_snapshot_rx,
                trigger_engine_node.clone(),
                trigger_engine_cancel.clone(),
            ));
        let event_source: Box<dyn crate::trigger_engine::TriggerSource> =
            Box::new(crate::trigger_engine::event_source::EventSource::new(
                trigger_engine_event_snapshot_rx,
                trigger_engine_node.clone(),
                trigger_engine_cancel.clone(),
            ));
        let subagent_source: Box<dyn crate::trigger_engine::TriggerSource> =
            Box::new(crate::trigger_engine::subagent_source::SubagentSource::new(
                trigger_engine_subagent_snapshot_rx,
                trigger_engine_node,
                trigger_engine_cancel.clone(),
            ));
        let manual_source_box: Box<dyn crate::trigger_engine::TriggerSource> =
            Box::new(manual_source);
        let sources: Vec<Box<dyn crate::trigger_engine::TriggerSource>> = vec![
            schedule_source,
            event_source,
            subagent_source,
            manual_source_box,
        ];
        let engine = crate::trigger_engine::TriggerEngine::new(
            trigger_engine_engine_snapshot_rx,
            materializer,
        );
        engine.run(sources, trigger_engine_cancel).await;
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

    let completion_node = agent.node.clone();
    let completion_agent_did = agent.agent_did().to_string();
    let completion_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::SubagentCompletion(
            crate::background_completion::run_background_completion_observer(
                completion_node,
                completion_agent_did,
                completion_cancel,
            )
            .await,
        )
    });

    let cancel_mirror_node = agent.node.clone();
    let cancel_mirror_snapshot_rx = active_snapshot_rx.clone();
    let cancel_mirror_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::CrossDeploymentCancelMirror(
            crate::trigger_engine::cross_deployment_cancel_mirror::run_cross_deployment_cancel_mirror(
                cancel_mirror_node,
                cancel_mirror_snapshot_rx,
                cancel_mirror_cancel,
            )
            .await,
        )
    });

    let router_node = agent.node.clone();
    let router_agent_did = agent.agent_did().to_string();
    let router_active_snapshot_rx = active_snapshot_rx.clone();
    let router_shutdown = shutdown.clone();
    background_tasks.spawn(async move {
        BackgroundTaskResult::Router(
            super::router::run_router(
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
            super::router::run_router_generation_observer(
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
        let control_agent_did = agent.agent_did().to_string();
        let control_context = agent
            .document_runtime_context()
            .cloned()
            .expect("checked document runtime context");
        let control_tx = reconcile_tx.clone();
        let control_runtime_status = runtime_status.clone();
        let control_shutdown = shutdown.clone();
        background_tasks.spawn(async move {
            BackgroundTaskResult::Control(
                super::control_watcher::run_control_watcher(
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
            Ok(BackgroundTaskResult::SubagentCompletion(result)) => (result, false),
            Ok(BackgroundTaskResult::CrossDeploymentCancelMirror(result)) => (result, false),
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
    let _ = trigger_engine_handle.await;

    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::Shutdown);
    }
    runtime_status
        .set_process_state(ProcessLifecycleState::Shutdown)
        .await;

    result
}

async fn log_recovery(node: &defra_node::EmbeddedNode, agent_did: &str, default_behavior_id: &str) {
    let mut recovered_any = false;

    match ToolCallLifecycle::recover_all(node, agent_did).await {
        Ok(report) => {
            if report.tool_calls_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.tool_calls_recovered,
                    "recovered stuck tool calls"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                agent_did = %agent_did,
                error = %error,
                "startup tool-call recovery failed"
            );
        }
    }

    match InferenceCall::recover_all(node, agent_did).await {
        Ok(report) => {
            if report.calls_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.calls_recovered,
                    "recovered stale inference calls"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                agent_did = %agent_did,
                error = %error,
                "startup inference-call recovery failed"
            );
        }
    }

    match RequestLifecycle::recover_all(node, agent_did).await {
        Ok(report) => {
            if report.requests_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.requests_recovered,
                    "recovered stuck requests"
                );
            }
            if report.responses_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.responses_recovered,
                    "recovered stuck responses"
                );
            }
            if report.conversations_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    agent_did = %agent_did,
                    count = report.conversations_recovered,
                    "recovered stuck conversations"
                );
            }
        }
        Err(error) => {
            tracing::warn!(agent_did = %agent_did, error = %error, "startup recovery failed");
        }
    }

    if !recovered_any {
        tracing::debug!(
            agent_did = %agent_did,
            default_behavior_id = %default_behavior_id,
            "startup recovery found no stuck documents"
        );
    }
}

fn is_degraded_startup_unavailable_reason(reason: &str) -> bool {
    let reason = reason.trim();
    reason.ends_with(" is disabled")
        || (reason.contains(" backend ")
            && reason.contains(" is unavailable (enabled=")
            && reason.contains(" probe_status="))
        || reason.contains("did not advertise model")
        || reason.contains("startup readiness probe")
        // A behavior with no backend binding is unconfigured, not structurally
        // invalid. Starting degraded (with /healthz reporting the reason and a
        // zero runnable count) lets an operator inspect and finish configuration
        // — applying a manifest, attaching a backend — over a live endpoint.
        // Treating it as fatal instead crash-loops the runtime, which is how a
        // fresh-store bootstrap (where ensure_agent_principal seeds a backendless
        // default behavior) became unstartable.
        || reason.contains("has no backend binding")
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
            .map(|(behavior_id, reason)| (behavior_id.clone(), reason.clone()))
            .collect::<Vec<_>>();
        unavailable.sort_by(|left, right| left.0.cmp(&right.0));

        if unavailable.is_empty() {
            anyhow::bail!(
                "agent {} has no runnable behaviors at startup",
                agent.agent_did()
            );
        }

        let blocking = unavailable
            .iter()
            .filter(|(_, reason)| !is_degraded_startup_unavailable_reason(reason))
            .map(|(behavior_id, reason)| format!("{behavior_id}: {reason}"))
            .collect::<Vec<_>>();
        if !blocking.is_empty() {
            anyhow::bail!(
                "agent {} has no runnable behaviors at startup due to invalid configuration ({})",
                agent.agent_did(),
                blocking.join("; ")
            );
        }
    }

    let mut behavior_ids = snapshot.behaviors.keys().cloned().collect::<Vec<_>>();
    behavior_ids.sort();

    for behavior_id in behavior_ids {
        let tool_surface = snapshot
            .tool_surfaces
            .get(&behavior_id)
            .ok_or_else(|| anyhow!("missing tool surface for behavior {behavior_id}"))?;
        tool_surface
            .build_tools(tool_runtime)
            .with_context(|| format!("building startup tool surface for behavior {behavior_id}"))?;
    }

    Ok(())
}

async fn resolve_tool_surfaces(
    node: &defra_node::EmbeddedNode,
    behaviors: &[Arc<crate::config::AgentBehavior>],
) -> Result<HashMap<String, Arc<ToolSurface>>> {
    let mut tool_surfaces = HashMap::with_capacity(behaviors.len());
    for behavior in behaviors {
        let tool_surface = behavior.tools.resolve(node).await?;
        tool_surfaces.insert(behavior.behavior_id.clone(), Arc::new(tool_surface));
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
            let backend_admission_configs =
                resolve_backend_admission_configs(agent.node.as_ref(), &agent.behaviors).await?;
            let paired_peer_dids = load_startup_paired_peer_dids(agent.node.as_ref()).await?;
            Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
                agent.default_behavior_id().to_string(),
                agent.behaviors.clone(),
                tool_surfaces,
                backend_admission_configs,
                agent.unavailable_behaviors.clone(),
            ))
            .map(|snapshot| {
                snapshot
                    .with_principal(agent.principal_arc())
                    .with_local_did(agent.agent_did().to_string())
                    .with_paired_peer_dids(paired_peer_dids)
            })
        }
    }
}

#[derive(Debug, Deserialize)]
struct StartupPeerPairingDesiredRow {
    peer_id: String,
    agent_did: Option<String>,
}

async fn load_startup_paired_peer_dids(node: &defra_node::EmbeddedNode) -> Result<HashSet<String>> {
    let query = r#"{
        PeerPairingDesired {
            peer_id
            agent_did
        }
    }"#;
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query PeerPairingDesired for startup paired peer DIDs failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<StartupPeerPairingDesiredRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("PeerPairingDesired"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.agent_did
                .as_deref()
                .map(str::trim)
                .filter(|did| !did.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    let peer_id = row.peer_id.trim();
                    peer_id.starts_with("did:").then(|| peer_id.to_string())
                })
        })
        .collect())
}

async fn resolve_backend_admission_configs(
    node: &defra_node::EmbeddedNode,
    behaviors: &[Arc<crate::config::AgentBehavior>],
) -> Result<HashMap<String, BackendAdmissionConfig>> {
    let mut configs = HashMap::new();
    for behavior in behaviors {
        let Some(backend_id) = behavior
            .backend_id
            .as_deref()
            .map(str::trim)
            .filter(|backend_id| !backend_id.is_empty())
        else {
            continue;
        };
        if configs.contains_key(backend_id) {
            continue;
        }
        let backend = backend_registry::lookup_backend(node, backend_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "behavior {} references missing backend {}",
                    behavior.behavior_id,
                    backend_id
                )
            })?;
        configs.insert(
            backend.backend_id.clone(),
            BackendAdmissionConfig::from_backend(&backend)?,
        );
    }
    Ok(configs)
}

async fn resolve_document_snapshot_with_tools(
    node: &defra_node::EmbeddedNode,
    resolve_context: &DocumentResolveContext,
) -> Result<ResolvedRuntimeSnapshot> {
    crate::agent::resolve_document_runtime_snapshot(node, resolve_context).await
}

#[cfg(test)]
mod degraded_reason_tests {
    use super::is_degraded_startup_unavailable_reason;

    #[test]
    fn unprobed_backend_is_degraded() {
        assert!(is_degraded_startup_unavailable_reason(
            "behavior 'default' backend workstation-1 is unavailable (enabled=true probe_status=unknown)"
        ));
    }

    #[test]
    fn disabled_behavior_is_degraded() {
        assert!(is_degraded_startup_unavailable_reason(
            "behavior 'x' is disabled"
        ));
    }

    #[test]
    fn no_backend_binding_is_degraded() {
        // A backendless behavior (e.g. the seeded bootstrap default before a
        // backend is configured) must not be fatal at startup.
        assert!(is_degraded_startup_unavailable_reason(
            "behavior did:key:zABC:default has no backend binding"
        ));
    }

    #[test]
    fn unknown_structural_reason_is_blocking() {
        assert!(!is_degraded_startup_unavailable_reason(
            "behavior 'default' references missing tool selection 'gone'"
        ));
    }
}
