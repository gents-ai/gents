use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::context::StartupBarrier;
use crate::inference::admission::AdmissionRegistry;
use crate::inference::backends::registry;
use crate::runtime::agent::reconcile::GenerationSupervisor;
use crate::runtime::agent::{Gents, ProcessLifecycleState};
use crate::runtime::status::RuntimeStatusHandle;
use crate::tools::mcp::health::ServiceHealthMap;
use crate::tools::surface::ToolRuntimeContext;
use gents_protocol::node_readiness::ReconcilePhase;

mod lifecycle_coordinator;
mod recovery;
mod shutdown;
mod slot_failure_policy;
pub(super) mod snapshot;
mod tasks;

use self::lifecycle_coordinator::RuntimeLifecycleCoordinator;
use self::recovery::log_recovery;
use self::shutdown::{shutdown_runtime, OwnedRuntimeTasks};
use self::slot_failure_policy::StartupSlotFailurePolicy;
use self::snapshot::{resolve_startup_snapshot, validate_startup_snapshot};
use self::tasks::StartupTasks;

enum BackgroundTaskResult {
    Router(Result<()>),
    ExecutorStatus(Result<()>),
    Reconcile(Result<()>),
    Control(Result<()>),
    BackgroundCompletion(Result<()>),
    PairingReconcile(Result<()>),
    EnrollmentReconcile(Result<()>),
    RegistryHeartbeat(Result<()>),
    EndpointHeartbeat(Result<()>),
    SessionCommandReconcile(Result<()>),
    ToolControlReconcile(Result<()>),
    DirectoryProjection(Result<()>),
}

impl BackgroundTaskResult {
    fn into_result(self) -> Result<()> {
        match self {
            Self::Router(result) => result.context("router task"),
            Self::ExecutorStatus(result) => result.context("executor status task"),
            Self::Reconcile(result) => result.context("generation supervisor task"),
            Self::Control(result) => result.context("control watcher task"),
            Self::BackgroundCompletion(result) => result.context("background completion task"),
            Self::PairingReconcile(result) => result.context("pairing reconcile task"),
            Self::EnrollmentReconcile(result) => result.context("enrollment reconcile task"),
            Self::RegistryHeartbeat(result) => result.context("registry heartbeat task"),
            Self::EndpointHeartbeat(result) => result.context("endpoint heartbeat task"),
            Self::SessionCommandReconcile(result) => {
                result.context("session command reconcile task")
            }
            Self::ToolControlReconcile(result) => result.context("tool control reconcile task"),
            Self::DirectoryProjection(result) => result.context("directory projection task"),
        }
    }
}

#[cfg(test)]
pub(super) type TestSlotRunner = Arc<
    dyn Fn(
            u64,
            watch::Receiver<bool>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

pub(in crate::runtime::agent) async fn run_agent(
    agent: Gents,
    external_shutdown: watch::Receiver<bool>,
) -> Result<()> {
    agent.backend_admission.validate(&agent.node).await?;
    crate::store::schema::migration::ensure_all_runtime_migrations(agent.node.clone())
        .await
        .context("ensure runtime schema migrations")?;
    let (runtime_status_owner, runtime_status) =
        RuntimeStatusHandle::start(agent.node.clone(), agent.node_did().to_string());
    run_agent_with_runtime_status(
        agent,
        external_shutdown,
        runtime_status_owner,
        runtime_status,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
pub(super) async fn run_agent_with_readiness_writer(
    agent: Gents,
    external_shutdown: watch::Receiver<bool>,
    writer: Arc<dyn crate::runtime::node_readiness_publisher::NodeReadinessWriter>,
    retry_delay: std::time::Duration,
) -> Result<()> {
    run_agent_with_readiness_writer_and_slot_runner(
        agent,
        external_shutdown,
        writer,
        retry_delay,
        None,
    )
    .await
}

#[cfg(test)]
pub(super) async fn run_agent_with_readiness_writer_and_slot_runner(
    agent: Gents,
    external_shutdown: watch::Receiver<bool>,
    writer: Arc<dyn crate::runtime::node_readiness_publisher::NodeReadinessWriter>,
    retry_delay: std::time::Duration,
    slot_runner: Option<TestSlotRunner>,
) -> Result<()> {
    crate::store::schema::migration::ensure_all_runtime_migrations(agent.node.clone())
        .await
        .context("ensure runtime schema migrations")?;
    let (runtime_status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        agent.node.clone(),
        agent.node_did().to_string(),
        writer,
        retry_delay,
    );
    run_agent_with_runtime_status(
        agent,
        external_shutdown,
        runtime_status_owner,
        runtime_status,
        slot_runner,
    )
    .await
}

async fn run_agent_with_runtime_status(
    agent: Gents,
    external_shutdown: watch::Receiver<bool>,
    runtime_status_owner: crate::runtime::status::RuntimeStatusOwner,
    runtime_status: RuntimeStatusHandle,
    #[cfg(test)] slot_runner: Option<TestSlotRunner>,
) -> Result<()> {
    let terminal_observer = agent.process_state_observer.clone();
    let initialized = runtime_status
        .initialize_startup(agent.default_agent_did())
        .await;
    let body_result = match initialized {
        Ok(()) => {
            run_agent_owned(
                agent,
                external_shutdown,
                runtime_status.clone(),
                #[cfg(test)]
                slot_runner,
            )
            .await
        }
        Err(error) => Err(error.context("initialize runtime agent_config readiness")),
    };

    finish_run_agent(
        body_result,
        runtime_status_owner,
        runtime_status,
        terminal_observer,
    )
    .await
}

async fn finish_run_agent(
    body_result: Result<()>,
    runtime_status_owner: crate::runtime::status::RuntimeStatusOwner,
    runtime_status: RuntimeStatusHandle,
    terminal_observer: Option<Arc<dyn crate::runtime::agent::ProcessLifecycleObserver>>,
) -> Result<()> {
    let mut teardown_error = None;
    let process_state = runtime_status.readiness().observation().process_state();
    if process_state != gents_protocol::node_readiness::NodeReadinessProcessState::Shutdown {
        if process_state != gents_protocol::node_readiness::NodeReadinessProcessState::ShuttingDown
        {
            if let Err(error) = runtime_status
                .set_process_state_durable(ProcessLifecycleState::ShuttingDown)
                .await
            {
                tracing::error!(error = %error, "failed to publish terminal ShuttingDown state");
                teardown_error = Some(error);
            } else if let Some(observer) = &terminal_observer {
                observer.on_process_state_change(ProcessLifecycleState::ShuttingDown);
            }
        }
        if let Err(error) = runtime_status
            .set_process_state_durable(ProcessLifecycleState::Shutdown)
            .await
        {
            tracing::error!(error = %error, "failed to publish terminal Shutdown state");
            if teardown_error.is_none() {
                teardown_error = Some(error);
            }
        } else if let Some(observer) = &terminal_observer {
            observer.on_process_state_change(ProcessLifecycleState::Shutdown);
        }
    }
    if let Err(error) = runtime_status_owner.close().await {
        tracing::error!(error = %error, "failed to close runtime agent_config readiness owner");
        if teardown_error.is_none() {
            teardown_error = Some(error);
        }
    }

    match (body_result, teardown_error) {
        (Err(error), Some(teardown_error)) => Err(error.context(format!(
            "runtime status teardown also failed: {teardown_error:#}"
        ))),
        (Err(error), None) => Err(error),
        (Ok(()), Some(error)) => Err(error),
        (Ok(()), None) => Ok(()),
    }
}

async fn resolve_startup(
    agent: &Gents,
    tool_runtime: &ToolRuntimeContext,
    runtime_status: &RuntimeStatusHandle,
) -> Result<(
    Option<super::control_watcher::ControlSubscription>,
    crate::triggers::runtime_snapshot::ResolvedRuntimeSnapshot,
)> {
    // Subscribe before reading startup configuration so racing commits remain pending.
    let control_subscription = match agent
        .document_runtime_context()
        .map(|_| super::control_watcher::subscribe_control_changes(&agent.node))
        .transpose()
    {
        Ok(subscription) => subscription,
        Err(error) => {
            runtime_status.publish_error(&format!("{error:#}")).await;
            return Err(error);
        }
    };
    let mut resolved_snapshot = match resolve_startup_snapshot(agent).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            runtime_status.publish_error(&format!("{error:#}")).await;
            return Err(error);
        }
    };
    if let Err(error) = validate_startup_snapshot(agent, tool_runtime, &mut resolved_snapshot).await
    {
        runtime_status.publish_error(&format!("{error:#}")).await;
        return Err(error);
    }
    log_recovery(
        &agent.node,
        agent.node_did(),
        agent.default_agent_did(),
        &resolved_snapshot
            .agents
            .values()
            .map(|agent| agent.agent_identity.clone())
            .chain(
                resolved_snapshot
                    .unavailable_agents
                    .values()
                    .filter_map(|unavailable| {
                        unavailable
                            .watcher_identity
                            .as_ref()
                            .map(|identity| identity.0.clone())
                    }),
            )
            .collect::<Vec<_>>(),
        &agent.background_execution_registry,
    )
    .await;
    for (agent_did, reason) in &agent.unavailable_agents {
        tracing::warn!(
            agent_did = %agent_did,
            public_reason = ?reason.public_reason,
            diagnostic = %reason.diagnostic,
            "agent_config unavailable at startup"
        );
    }
    Ok((control_subscription, resolved_snapshot))
}

async fn run_agent_owned(
    agent: Gents,
    mut external_shutdown: watch::Receiver<bool>,
    runtime_status: RuntimeStatusHandle,
    #[cfg(test)] slot_runner: Option<TestSlotRunner>,
) -> Result<()> {
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let (fatal_error_tx, mut fatal_error_rx) = mpsc::unbounded_channel();
    let (runtime_shutdown_tx, shutdown) = watch::channel(false);
    runtime_status
        .set_reconcile_phase(ReconcilePhase::Resolving)
        .await;
    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::Recovering);
    }
    let health_map = ServiceHealthMap::new();
    let tool_runtime = ToolRuntimeContext::new_with_node_did(
        agent.node.clone(),
        agent.mcp_pool.clone(),
        health_map.clone(),
        agent.local_hostname.clone(),
        agent.local_subnet.clone(),
        agent.node_did().to_string(),
        Some(agent.runtime_node_arc().identity.clone()),
    )
    .with_plugins(agent.plugins().clone())
    .with_usage_cache(agent.usage_cache().clone());
    registry::probe_and_promote_enabled_backends(&agent.node).await;

    let (control_subscription, resolved_snapshot) =
        resolve_startup(&agent, &tool_runtime, &runtime_status).await?;

    let startup_barrier = Arc::new(StartupBarrier::new(
        &resolved_snapshot
            .agents
            .values()
            .cloned()
            .collect::<Vec<_>>(),
    ));
    #[cfg(not(test))]
    let admission_gate = super::router::RuntimeAdmissionGate::closed();
    #[cfg(test)]
    let admission_gate = super::router::RuntimeAdmissionGate::closed_with_dispatch_probe(
        agent.router_dispatch_probe.clone(),
    );
    let lifecycle = Arc::new(RuntimeLifecycleCoordinator::new(
        admission_gate.clone(),
        runtime_status.clone(),
        agent.process_state_observer.clone(),
    ));
    let admission_registry = AdmissionRegistry::new(agent.node.clone());
    let (enrollment_owner, enrollment_handle) =
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    *agent
        .enrollment_authority
        .write()
        .map_err(|_| anyhow::anyhow!("enrollment authority is unavailable"))? =
        Some(enrollment_handle.clone());
    let lsp_pool = tool_runtime.lsp_pool.clone();
    let control_tool_runtime = tool_runtime.clone();
    let runtime = tasks::runtime_context(
        &agent,
        tool_runtime,
        &admission_registry,
        &startup_barrier,
        &runtime_status,
        &enrollment_handle,
    );
    let runtime_for_runner = runtime.clone();
    #[cfg(test)]
    let test_slot_runner = slot_runner;
    let readiness_fatal_error = fatal_error_tx.clone();
    let slot_failure_policy: Arc<dyn crate::runtime::agent::reconcile::SlotFailurePolicy> =
        Arc::new(StartupSlotFailurePolicy {
            barrier: startup_barrier.clone(),
            runtime_status: runtime_status.clone(),
            admission_gate: admission_gate.clone(),
            runtime_cancel: cancel.clone(),
            fatal_error: fatal_error_tx,
            observer: agent.startup_build_failure_observer.clone(),
            budget: agent.startup_readiness.build_failure_budget,
        });
    let generation_supervisor = GenerationSupervisor::bootstrap(
        resolved_snapshot,
        admission_registry.clone(),
        agent.retry_policy.clone(),
        move |agent, tool_surface, request_rx, capacity, generation, shutdown| {
            let runtime = runtime_for_runner.clone();
            #[cfg(test)]
            let test_slot_runner = test_slot_runner.clone();
            async move {
                #[cfg(test)]
                if let Some(test_slot_runner) = test_slot_runner {
                    return test_slot_runner(generation, shutdown.clone()).await;
                }
                runtime
                    .run_agent(
                        agent,
                        tool_surface,
                        request_rx,
                        capacity,
                        generation,
                        shutdown,
                    )
                    .await
            }
        },
        runtime_status.clone(),
        shutdown.clone(),
        Some(slot_failure_policy),
    )
    .await?;
    let initial_active_snapshot = generation_supervisor.current_snapshot();
    if let Err(error) = runtime_status
        .publish_startup_snapshot(initial_active_snapshot.as_ref())
        .await
    {
        runtime_shutdown_tx.send_replace(true);
        cancel.cancel();
        let shutdown_result = generation_supervisor.shutdown_slots().await;
        return match shutdown_result {
            Ok(()) => Err(error).context("durably publish startup agent_config readiness source"),
            Err(shutdown_error) => Err(error).context(format!(
                "durably publish startup agent_config readiness source; agent_config slot shutdown also failed: {shutdown_error:#}"
            )),
        };
    }
    let (active_snapshot_tx, active_snapshot_rx) = watch::channel(initial_active_snapshot.clone());
    *agent
        .active_snapshot
        .write()
        .map_err(|_| anyhow::anyhow!("runtime snapshot binding unavailable"))? =
        Some(active_snapshot_rx.clone());
    runtime.job_driver.runtime().bind_snapshot(
        crate::triggers::runtime_snapshot::ActiveSnapshotAgentIdentities::boxed(active_snapshot_rx.clone()),
    )?;
    let (reconcile_tx, reconcile_rx) = mpsc::channel(8);
    let _reconcile_tx_guard = reconcile_tx.clone();
    let (health_checker, backend_prober, backend_health_events_rx) =
        tasks::spawn_health_probes(&agent, &health_map, &cancel);

    let startup_tasks = StartupTasks {
        agent: &agent,
        cancel: &cancel,
        shutdown: &shutdown,
        runtime_status: &runtime_status,
        active_snapshot_rx: &active_snapshot_rx,
        startup_barrier: &startup_barrier,
    };
    let runtime_snapshot_observer_handle = tasks::spawn_snapshot_observer(startup_tasks);
    let trigger_engine_handle = tasks::spawn_trigger_engine(startup_tasks);
    let callback_engine_handle = tasks::spawn_callback_engine(startup_tasks);
    let readiness_handle = tasks::spawn_readiness(
        startup_tasks,
        &lifecycle,
        &initial_active_snapshot,
        readiness_fatal_error,
    );

    let mut background_tasks = JoinSet::new();

    tasks::spawn_node_tasks(
        startup_tasks,
        &mut background_tasks,
        &runtime,
        enrollment_owner,
        &enrollment_handle,
    );
    tasks::spawn_request_tasks(startup_tasks, &mut background_tasks, &admission_gate);

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

    tasks::spawn_control_watcher(
        startup_tasks,
        &mut background_tasks,
        control_subscription,
        &reconcile_tx,
        control_tool_runtime,
        backend_health_events_rx,
    );

    let service_admission = agent.service_admission();
    let result = tokio::select! {
        biased;
        Some(error) = fatal_error_rx.recv() => Err(error),
        _ = external_shutdown.changed() => Ok(()),
        _ = service_admission.retirement_requested() => Ok(()),
        Some(joined) = background_tasks.join_next() => match joined {
            Ok(task) => task.into_result(),
            Err(error) => Err(anyhow!("background task join failed: {error}")),
        },
        else => Ok(()),
    };

    shutdown_runtime(
        result,
        &agent,
        &lifecycle,
        &runtime_shutdown_tx,
        &cancel,
        OwnedRuntimeTasks {
            background_tasks,
            readiness_handle,
            trigger_engine_handle,
            callback_engine_handle,
            health_checker,
            backend_prober,
            runtime_snapshot_observer_handle,
        },
        lsp_pool,
    )
    .await
}

#[cfg(test)]
mod degraded_reason_tests;

#[cfg(test)]
mod startup_slot_failure_policy_tests;

#[cfg(test)]
mod lifecycle_coordinator_tests;
