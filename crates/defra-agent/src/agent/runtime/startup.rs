use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::context::{RuntimeContext, StartupBarrier};
use crate::admission::{AdmissionRegistry, BackendAdmissionConfig};
use crate::agent::reconcile::GenerationSupervisor;
use crate::agent::{DefraAgent, DocumentResolveContext, ProcessLifecycleState};
use crate::backend_registry;
use crate::health_checker::{spawn_health_checker, ServiceHealthMap};
use crate::lifecycle::RequestLifecycle;
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};
use crate::tool_surface::{ToolRuntimeContext, ToolSurface};

enum BackgroundTaskResult {
    Router(Result<()>),
    RouterObserver(Result<()>),
    Reconcile(Result<()>),
    Control(Result<()>),
}

pub(in crate::agent) async fn run_agent(
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
    let admission_registry = AdmissionRegistry::new(agent.node.clone());
    let runtime = RuntimeContext {
        node: agent.node.clone(),
        tool_runtime,
        admission_registry: admission_registry.clone(),
        retry_policy: agent.retry_policy.clone(),
        hook_failure_policy: agent.hook_failure_policy,
        startup_barrier: startup_barrier.clone(),
    };
    let scheduler_tool_runtime = runtime.tool_runtime.clone();
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

    let scheduler = crate::scheduler::Scheduler::new(
        agent.node.clone(),
        active_snapshot_rx.clone(),
        scheduler_tool_runtime,
        admission_registry.clone(),
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
                super::run_control_watcher(
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

fn is_degraded_startup_unavailable_reason(reason: &str) -> bool {
    let reason = reason.trim();
    reason.ends_with(" is disabled")
        || (reason.contains(" backend ")
            && reason.contains(" is unavailable (enabled=")
            && reason.contains(" probe_status="))
        || reason.contains("did not advertise model")
        || reason.contains("startup readiness probe")
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
            let backend_admission_configs =
                resolve_backend_admission_configs(agent.node.as_ref(), &agent.behaviors).await?;
            Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
                agent.default_behavior_id.clone(),
                agent.behaviors.clone(),
                tool_surfaces,
                backend_admission_configs,
                agent.unavailable_behaviors.clone(),
            ))
        }
    }
}

async fn resolve_backend_admission_configs(
    node: &defra_node::EmbeddedNode,
    behaviors: &[Arc<crate::config::BehaviorConfig>],
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
                    behavior.name,
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
