use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::context::StartupBarrier;
use crate::runtime::agent::reconcile::GenerationSupervisor;
use crate::runtime::agent::Gents;
use crate::runtime::status::RuntimeStatusHandle;
use gents_inference::admission::AdmissionRegistry;
use gents_inference::backends::registry;
use gents_protocol::node_readiness::ReconcilePhase;
use gents_tools::mcp::health::ServiceHealthMap;
use gents_tools::surface::ToolRuntimeContext;

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
    gents_store::schema::catalog::ensure_runtime_owned(agent.node.clone())
        .await
        .context("ensure runtime schemas")?;
    let (runtime_status_owner, runtime_status) = RuntimeStatusHandle::start(
        agent.node.clone(),
        agent.node_did().to_string(),
        agent.storage_fault(),
    );
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
    gents_store::schema::catalog::ensure_runtime_owned(agent.node.clone())
        .await
        .context("ensure runtime schemas")?;
    let (runtime_status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        agent.node.clone(),
        agent.node_did().to_string(),
        writer,
        retry_delay,
        agent.storage_fault(),
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
    let initialized = runtime_status
        .initialize_startup(agent.default_agent_did())
        .await;
    let body_result = match initialized {
        Ok(()) => {
            run_agent_owned(
                agent,
                external_shutdown,
                runtime_status,
                &lifecycle,
                admission_gate,
                #[cfg(test)]
                slot_runner,
            )
            .await
        }
        Err(error) => Err(error.context("initialize runtime agent_config readiness")),
    };

    finish_run_agent(body_result, runtime_status_owner, &lifecycle).await
}

async fn finish_run_agent(
    body_result: Result<()>,
    runtime_status_owner: crate::runtime::status::RuntimeStatusOwner,
    lifecycle: &RuntimeLifecycleCoordinator,
) -> Result<()> {
    let mut teardown_error = lifecycle.finish_run().await.err();
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
    gents_automation::triggers::runtime_snapshot::ResolvedRuntimeSnapshot,
    bool,
)> {
    resolve_startup_until(
        agent,
        tool_runtime,
        runtime_status,
        tokio::time::Instant::now() + gents_tools::surface::PUBLICATION_WAIT,
    )
    .await
}

/// Startup's configuration, read under the plugin store's publication lease
/// when it selects plugins, waiting for an installer until `deadline`, and
/// whether startup recovery found stuck requests.
pub(super) async fn resolve_startup_until(
    agent: &Gents,
    tool_runtime: &ToolRuntimeContext,
    runtime_status: &RuntimeStatusHandle,
    deadline: tokio::time::Instant,
) -> Result<(
    Option<super::control_watcher::ControlSubscription>,
    gents_automation::triggers::runtime_snapshot::ResolvedRuntimeSnapshot,
    bool,
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
    let publication = if snapshot::depends_on_publication(&resolved_snapshot) {
        let lease = match gents_tools::surface::publication_guard_until(
            tool_runtime.plugin_store_pin(),
            deadline,
        )
        .await
        {
            Ok(guard) => guard,
            Err(gents_pack::plugin::store::PublishedPluginReadError::Busy) => None,
            Err(gents_pack::plugin::store::PublishedPluginReadError::Absent(
                gents_pack::plugin::store::StoreAbsent::Missing,
            )) if tool_runtime
                .plugin_store_pin()
                .is_some_and(|pin| !pin.is_captured()) =>
            {
                None
            }
            Err(error) => {
                runtime_status.publish_error(&format!("{error:#}")).await;
                return Err(error.into());
            }
        };
        // Re-resolve after the wait, held or not: a change committed while
        // startup waited must not wait for the watcher's own epoch.
        resolved_snapshot = match resolve_startup_snapshot(agent).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                runtime_status.publish_error(&format!("{error:#}")).await;
                return Err(error);
            }
        };
        lease
    } else {
        None
    };
    if let Err(error) =
        validate_startup_snapshot(agent, tool_runtime, &mut resolved_snapshot, deadline).await
    {
        runtime_status.publish_error(&format!("{error:#}")).await;
        return Err(error);
    }
    drop(publication);
    if resolved_snapshot
        .unavailable_agents
        .values()
        .any(|unavailable| unavailable.publication_busy)
    {
        runtime_status.request_publication_recovery();
    }
    let stuck_requests = log_recovery(
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
    Ok((control_subscription, resolved_snapshot, stuck_requests))
}

async fn run_agent_owned(
    agent: Gents,
    mut external_shutdown: watch::Receiver<bool>,
    runtime_status: RuntimeStatusHandle,
    lifecycle: &Arc<RuntimeLifecycleCoordinator>,
    admission_gate: super::router::RuntimeAdmissionGate,
    #[cfg(test)] slot_runner: Option<TestSlotRunner>,
) -> Result<()> {
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let (fatal_error_tx, mut fatal_error_rx) = mpsc::unbounded_channel();
    let (runtime_shutdown_tx, shutdown) = watch::channel(false);
    runtime_status
        .set_reconcile_phase(ReconcilePhase::Resolving)
        .await;
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
    .with_tasks(Arc::new(gents_automation::triggers::task_run::TaskRuns))
    .with_registry_url(agent.registry_url().map(str::to_owned))
    .with_usage_cache(agent.usage_cache().clone())
    .with_native_executables(agent.native_executables().clone());
    registry::probe_and_promote_enabled_backends(&agent.node).await;

    let (control_subscription, resolved_snapshot, stuck_requests) = tokio::select! {
        biased;
        _ = external_shutdown.changed() => return Ok(()),
        resolved = resolve_startup(&agent, &tool_runtime, &runtime_status) => resolved?,
    };
    lifecycle.startup_recovered(stuck_requests).await?;

    let startup_barrier = Arc::new(StartupBarrier::new(
        &resolved_snapshot
            .agents
            .values()
            .cloned()
            .collect::<Vec<_>>(),
    ));
    let admission_registry = AdmissionRegistry::new(agent.node.clone());
    let (enrollment_owner, enrollment_handle) =
        gents_p2p::p2p_reconcile::enrollment_authority_channel();
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
    .await?
    .with_observer(agent.runtime_snapshot_observer.clone());
    let initial_active_snapshot = generation_supervisor.current_snapshot();
    if let Err(error) = runtime_status
        .publish_startup_snapshot(initial_active_snapshot.as_ref())
        .await
    {
        drop(initial_active_snapshot);
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
    // A snapshot's dispatchers hold its slots' request channels open, and a
    // retired slot exits only once they close: the watch takes the only
    // startup copy, so replacing it releases generation 1.
    let (active_snapshot_tx, active_snapshot_rx) = watch::channel(initial_active_snapshot);
    runtime.job_driver.runtime().bind_snapshot(
        gents_automation::triggers::runtime_snapshot::ActiveSnapshotAgentIdentities::boxed(
            active_snapshot_rx.clone(),
        ),
    )?;
    let (reconcile_tx, reconcile_rx) = mpsc::channel(8);
    let _reconcile_tx_guard = reconcile_tx.clone();
    let (health_checker, backend_prober, backend_health_events_rx) =
        tasks::spawn_health_probes(&agent, &health_map, &admission_registry, &cancel);

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
    let readiness_handle = tasks::spawn_readiness(startup_tasks, lifecycle, readiness_fatal_error);

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
        lifecycle,
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
            admission_registry,
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
