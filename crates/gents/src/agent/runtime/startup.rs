use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::context::{RuntimeContext, StartupBarrier};
use crate::admission::AdmissionRegistry;
use crate::agent::reconcile::GenerationSupervisor;
use crate::agent::{Gents, ProcessLifecycleState};
use crate::backend_registry;
use crate::health_checker::{spawn_health_checker, ServiceHealthMap};
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};
use crate::tool_surface::ToolRuntimeContext;

mod lifecycle_coordinator;
mod recovery;
mod slot_failure_policy;
pub(super) mod snapshot;

use self::lifecycle_coordinator::RuntimeLifecycleCoordinator;
use self::recovery::log_recovery;
use self::slot_failure_policy::StartupSlotFailurePolicy;
use self::snapshot::{resolve_startup_snapshot, validate_startup_snapshot};

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
    SessionHydrationReconcile(Result<()>),
    SessionCommandReconcile(Result<()>),
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
            Self::SessionHydrationReconcile(result) => {
                result.context("session hydration reconcile task")
            }
            Self::SessionCommandReconcile(result) => {
                result.context("session command reconcile task")
            }
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

pub(in crate::agent) async fn run_agent(
    agent: Gents,
    external_shutdown: watch::Receiver<bool>,
) -> Result<()> {
    crate::migration::ensure_all_runtime_migrations(agent.node.clone())
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
    writer: Arc<dyn crate::node_readiness_publisher::NodeReadinessWriter>,
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
    writer: Arc<dyn crate::node_readiness_publisher::NodeReadinessWriter>,
    retry_delay: std::time::Duration,
    slot_runner: Option<TestSlotRunner>,
) -> Result<()> {
    crate::migration::ensure_all_runtime_migrations(agent.node.clone())
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
    runtime_status_owner: crate::runtime_status::RuntimeStatusOwner,
    runtime_status: RuntimeStatusHandle,
    #[cfg(test)] slot_runner: Option<TestSlotRunner>,
) -> Result<()> {
    let terminal_observer = agent.process_state_observer.clone();
    let initialized = runtime_status
        .initialize_startup(agent.default_agent_id())
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
    runtime_status_owner: crate::runtime_status::RuntimeStatusOwner,
    runtime_status: RuntimeStatusHandle,
    terminal_observer: Option<Arc<dyn crate::agent::ProcessLifecycleObserver>>,
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
    backend_registry::probe_and_promote_enabled_backends(&agent.node).await;

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
    let mut resolved_snapshot = match resolve_startup_snapshot(&agent).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            runtime_status.publish_error(&format!("{error:#}")).await;
            return Err(error);
        }
    };
    if let Err(error) =
        validate_startup_snapshot(&agent, &tool_runtime, &mut resolved_snapshot).await
    {
        runtime_status.publish_error(&format!("{error:#}")).await;
        return Err(error);
    }
    log_recovery(
        &agent.node,
        agent.node_did(),
        agent.default_agent_id(),
        &agent.background_execution_registry,
    )
    .await;
    for (agent_id, reason) in &agent.unavailable_agents {
        tracing::warn!(
            agent_id = %agent_id,
            public_reason = ?reason.public_reason,
            diagnostic = %reason.diagnostic,
            "agent_config unavailable at startup"
        );
    }

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
        crate::agent::p2p_reconcile::enrollment_authority_channel();
    *agent
        .enrollment_authority
        .write()
        .map_err(|_| anyhow::anyhow!("enrollment authority is unavailable"))? =
        Some(enrollment_handle.clone());
    let lsp_pool = tool_runtime.lsp_pool.clone();
    let control_tool_runtime = tool_runtime.clone();
    let job_driver = crate::plugin_job::driver::JobDriver::with_service_admission(
        crate::plugin_job::driver::JobRuntime {
            node: agent.node.clone(),
            plugins: agent.plugins().clone(),
            home: agent
                .plugins()
                .home()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default(),
            tool_root: agent.operator_tool_root().map(PathBuf::from),
        },
        agent.service_admission(),
    );
    let runtime = RuntimeContext {
        node: agent.node.clone(),
        tool_runtime,
        admission_registry: admission_registry.clone(),
        hook_failure_policy: agent.hook_failure_policy,
        rendered_request_capture_factory: agent.rendered_request_capture_factory.clone(),
        background_execution_registry: agent.background_execution_registry.clone(),
        inference_observations: agent.inference_observations.clone(),
        startup_barrier: startup_barrier.clone(),
        startup_readiness: agent.startup_readiness.clone(),
        runtime_status: runtime_status.clone(),
        operator_tool_root: agent.operator_tool_root().map(PathBuf::from),
        job_driver,
        enrollment_authority: enrollment_handle.clone(),
    };
    let runtime_for_runner = runtime.clone();
    #[cfg(test)]
    let test_slot_runner = slot_runner;
    let readiness_fatal_error = fatal_error_tx.clone();
    let slot_failure_policy: Arc<dyn crate::agent::reconcile::SlotFailurePolicy> =
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
    let (reconcile_tx, reconcile_rx) = mpsc::channel(8);
    let _reconcile_tx_guard = reconcile_tx.clone();
    let health_checker = AbortOnDropHandle::new(spawn_health_checker(
        agent.node.clone(),
        agent.mcp_pool.clone(),
        health_map.clone(),
        agent.local_hostname.clone(),
        agent.local_subnet.clone(),
        cancel.child_token(),
        agent.health_checker_options.clone(),
        agent.node_did().to_string(),
    ));
    let (backend_health_events_tx, backend_health_events_rx) = mpsc::channel::<()>(1);
    let backend_prober = AbortOnDropHandle::new(crate::backend_health::spawn_backend_prober(
        agent.node.clone(),
        agent.backend_health.clone(),
        agent.backend_prober_options.clone(),
        backend_health_events_tx,
        cancel.child_token(),
        agent.node_did().to_string(),
    ));

    let runtime_snapshot_observer_handle =
        if let Some(observer) = agent.runtime_snapshot_observer.clone() {
            let mut snapshot_rx = active_snapshot_rx.clone();
            let mut observer_shutdown = shutdown.clone();
            Some(AbortOnDropHandle::new(tokio::spawn(async move {
                loop {
                    let (generation, fingerprint, runnable) = {
                        let snapshot = snapshot_rx.borrow_and_update();
                        let mut ids: Vec<String> = snapshot.agents.keys().cloned().collect();
                        ids.sort();
                        (
                            snapshot.generation,
                            snapshot.configuration_fingerprint(),
                            ids,
                        )
                    };
                    observer.on_generation_published(generation, &fingerprint, &runnable);

                    tokio::select! {
                        changed = snapshot_rx.changed() => {
                            if changed.is_err() {
                                break;
                            }
                        }
                        _ = observer_shutdown.changed() => break,
                    }
                }
            })))
        } else {
            None
        };

    let trigger_engine_node = agent.node.clone();
    let trigger_engine_schedule_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_event_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_goal_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_engine_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_materializer_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_cancel = cancel.child_token();
    let trigger_engine_startup_barrier = startup_barrier.clone();
    let (manual_source, manual_trigger_handle) =
        crate::trigger_engine::manual_source::ManualSource::new(trigger_engine_cancel.clone());
    let _ = agent.manual_trigger_handle.set(manual_trigger_handle);
    let trigger_engine_node_did = agent.node_did().to_string();
    let trigger_engine_runtime_observer = agent.runtime_snapshot_observer.clone();
    let trigger_engine_handle = AbortOnDropHandle::new(tokio::spawn(async move {
        tokio::select! {
            _ = trigger_engine_cancel.cancelled() => return,
            _ = trigger_engine_startup_barrier.wait_ready() => {}
        }
        match crate::trigger_engine::production_materializer::recover_workspace_binding_pending_requests(
            &trigger_engine_node,
            &trigger_engine_node_did,
        )
        .await
        {
            Ok(recovered) if recovered > 0 => tracing::info!(
                recovered,
                "recovered workspace-binding-pending requests"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "workspace binding recovery sweep failed"),
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
        let event_source: Box<dyn crate::trigger_engine::TriggerSource> = Box::new(
            crate::trigger_engine::event_source::EventSource::new(
                trigger_engine_event_snapshot_rx,
                trigger_engine_node.clone(),
                trigger_engine_cancel.clone(),
            )
            .with_runtime_observer(trigger_engine_runtime_observer),
        );
        let goal_source: Box<dyn crate::trigger_engine::TriggerSource> =
            Box::new(crate::trigger_engine::goal_source::GoalSource::new(
                trigger_engine_goal_snapshot_rx,
                trigger_engine_node.clone(),
                trigger_engine_cancel.clone(),
            ));
        let manual_source_box: Box<dyn crate::trigger_engine::TriggerSource> =
            Box::new(manual_source);
        let sources: Vec<Box<dyn crate::trigger_engine::TriggerSource>> = vec![
            schedule_source,
            event_source,
            goal_source,
            manual_source_box,
        ];
        let engine = crate::trigger_engine::TriggerEngine::new(
            trigger_engine_engine_snapshot_rx,
            materializer,
        );
        engine.run(sources, trigger_engine_cancel).await;
    }));

    let callback_node = agent.node.clone();
    let callback_node_did = agent.node_did().to_string();
    let callback_ceiling = agent
        .document_runtime_context()
        .and_then(|context| context.tool_ceiling.root())
        .map(std::path::Path::to_path_buf);
    crate::workspace::install_process_operator_tool_root(callback_ceiling.clone());
    let callback_plugins = agent.plugins().clone();
    let callback_cancel = cancel.child_token();
    let callback_startup_barrier = startup_barrier.clone();
    let callback_engine_handle = AbortOnDropHandle::new(tokio::spawn(async move {
        tokio::select! {
            _ = callback_cancel.cancelled() => return,
            _ = callback_startup_barrier.wait_ready() => {}
        }
        if let Err(error) = crate::callback::run_callback_engine(
            callback_node,
            callback_node_did,
            callback_ceiling,
            callback_plugins,
            callback_cancel,
        )
        .await
        {
            tracing::error!(%error, "callback engine exited");
        }
    }));

    let ready_cancel = cancel.child_token();
    let ready_startup_barrier = startup_barrier.clone();
    let ready_lifecycle = lifecycle.clone();
    let ready_agent_count = initial_active_snapshot.agents.len();
    let ready_unavailable_count = initial_active_snapshot.unavailable_agents.len();
    let ready_runtime_status = runtime_status.clone();
    let readiness_handle = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut watchdog = tokio::time::interval(std::time::Duration::from_secs(60));
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        watchdog.tick().await;
        loop {
            tokio::select! {
                _ = ready_cancel.cancelled() => return,
                _ = ready_startup_barrier.wait_ready() => break,
                _ = watchdog.tick() => {
                    let pending = ready_startup_barrier.pending_agents().await;
                    tracing::warn!(
                        pending_agents = ?pending,
                        "startup readiness barrier is still waiting; a build may be wedged"
                    );
                }
            }
        }
        let became_ready = match ready_lifecycle.become_ready().await {
            Ok(ready) => ready,
            Err(error) => {
                tracing::error!(error = %error, "failed to durably publish runtime readiness");
                let _ = readiness_fatal_error
                    .send(error.context("durably publish runtime Ready state"));
                return;
            }
        };
        if !became_ready {
            return;
        }
        let demoted = ready_runtime_status.readiness().observation();
        let demoted = demoted.demotions();
        if demoted.is_empty() {
            tracing::info!(
                runnable_agents = ready_agent_count,
                unavailable_agents = ready_unavailable_count,
                "gents ready"
            );
        } else {
            let mut demoted_ids: Vec<&String> = demoted.keys().collect();
            demoted_ids.sort();
            tracing::warn!(
                runnable_agents = ready_agent_count.saturating_sub(demoted.len()),
                unavailable_agents = ready_unavailable_count + demoted.len(),
                demoted_agents = ?demoted_ids,
                "gents ready (degraded: startup build failures demoted agents)"
            );
        }
    }));

    let mut background_tasks = JoinSet::new();

    let completion_node = agent.node.clone();
    let completion_node_did = agent.node_did().to_string();
    let completion_background_executions = agent.background_execution_registry.clone();
    let completion_job_driver = runtime.job_driver.clone();
    let completion_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::BackgroundCompletion(
            crate::background_completion::run_background_completion_observer(
                completion_node,
                completion_node_did,
                completion_background_executions,
                completion_job_driver,
                completion_cancel,
            )
            .await,
        )
    });

    let enrollment_node = agent.node.clone();
    let enrollment_identity = agent.runtime_node_arc().identity.clone();
    let enrollment_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::EnrollmentReconcile(
            crate::agent::p2p_reconcile::run_enrollment_reconciler(
                enrollment_node,
                enrollment_identity,
                enrollment_owner,
                enrollment_cancel,
            )
            .await,
        )
    });

    let pairing_node = agent.node.clone();
    let pairing_identity = agent.runtime_node_arc().identity.clone();
    let pairing_enrollment = enrollment_handle.clone();
    let pairing_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::PairingReconcile(
            crate::agent::p2p_reconcile::run_pairing_reconciler(
                pairing_node,
                pairing_identity,
                pairing_enrollment,
                pairing_cancel,
            )
            .await,
        )
    });

    let registry_node = agent.node.clone();
    let registry_node_did = agent.node_did().to_string();
    let registry_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::RegistryHeartbeat(
            crate::agent::p2p_reconcile::run_registry_heartbeat(
                registry_node,
                registry_node_did,
                crate::agent::p2p_reconcile::resolve_network_id(),
                registry_cancel,
            )
            .await,
        )
    });

    let endpoint_node = agent.node.clone();
    let endpoint_identity = agent.runtime_node_arc().identity.clone();
    let endpoint_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::EndpointHeartbeat(
            crate::agent::p2p_reconcile::run_endpoint_heartbeat(
                endpoint_node,
                endpoint_identity,
                endpoint_cancel,
            )
            .await,
        )
    });

    let hydration_node = agent.node.clone();
    let hydration_enrollment = enrollment_handle.clone();
    let hydration_identity = agent.runtime_node_arc().identity.clone();
    let hydration_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::SessionHydrationReconcile(
            crate::agent::p2p_reconcile::run_session_hydration_reconciler(
                hydration_node,
                hydration_enrollment,
                hydration_identity,
                hydration_cancel,
            )
            .await,
        )
    });

    let session_command_node = agent.node.clone();
    let session_command_identity = agent.runtime_node_arc().identity.clone();
    let session_command_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::SessionCommandReconcile(
            crate::agent::p2p_reconcile::session_commands::run_session_command_reconciler(
                session_command_node,
                session_command_identity,
                session_command_cancel,
            )
            .await,
        )
    });

    let directory_node = agent.node.clone();
    let directory_source_did = agent.node_did().to_string();
    let directory_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::DirectoryProjection(
            crate::agent::directory_projection::run_directory_projection(
                directory_node,
                directory_source_did,
                directory_cancel,
            )
            .await,
        )
    });

    let router_node = agent.node.clone();
    let router_node_did = agent.node_did().to_string();
    let router_active_snapshot_rx = active_snapshot_rx.clone();
    let router_shutdown = shutdown.clone();
    let router_admission_gate = admission_gate.clone();
    let router_runtime_status = runtime_status.clone();
    let router_runtime_snapshot_observer = agent.runtime_snapshot_observer.clone();
    background_tasks.spawn(async move {
        BackgroundTaskResult::Router(
            super::router::run_router(
                router_node,
                router_node_did,
                router_active_snapshot_rx,
                router_shutdown,
                router_admission_gate,
                router_runtime_status,
                router_runtime_snapshot_observer,
            )
            .await,
        )
    });

    let executor_status_active_snapshot_rx = active_snapshot_rx.clone();
    let executor_status_runtime_status = runtime_status.clone();
    let executor_status_shutdown = shutdown.clone();
    background_tasks.spawn(async move {
        BackgroundTaskResult::ExecutorStatus(
            crate::runtime_status::run_executor_status_observer(
                executor_status_active_snapshot_rx,
                executor_status_runtime_status,
                executor_status_shutdown,
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
        let control_subscription =
            control_subscription.expect("document runtime context has control subscription");
        let control_node_did = agent.node_did().to_string();
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
                    super::control_watcher::ControlWatcherInputs {
                        node: control_node,
                        subscription: control_subscription,
                        node_did: control_node_did,
                        resolve_context: control_context,
                        tool_runtime: control_tool_runtime,
                        proposals_tx: control_tx,
                        runtime_status: control_runtime_status,
                        health_events_rx: backend_health_events_rx,
                        shutdown: control_shutdown,
                    },
                )
                .await,
            )
        });
    }

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

    let mut teardown_error = None;
    if let Err(error) = lifecycle.begin_shutdown().await {
        tracing::error!(error = %error, "failed to begin ordered runtime shutdown");
        teardown_error = Some(error);
    }

    runtime_shutdown_tx.send_replace(true);
    cancel.cancel();
    let task_hook_records = agent
        .background_execution_registry
        .task_hook_records()
        .clone();
    let task_hook_recoveries = tokio::spawn(async move { task_hook_records.shutdown().await });
    while let Some(joined) = background_tasks.join_next().await {
        let task_result = match joined {
            Ok(task) => task.into_result(),
            Err(error) => Err(anyhow!(
                "background task join failed during shutdown: {error}"
            )),
        };
        if let Err(error) = task_result {
            tracing::error!(error = %error, "background task exited during shutdown");
            if teardown_error.is_none() {
                teardown_error = Some(error);
            }
        }
    }

    for (name, joined) in [
        ("task hook recovery", task_hook_recoveries.await),
        ("readiness", readiness_handle.await),
        ("trigger engine", trigger_engine_handle.await),
        ("callback engine", callback_engine_handle.await),
        ("health checker", health_checker.await),
        ("backend prober", backend_prober.await),
    ] {
        if let Err(error) = joined {
            tracing::error!(name, error = %error, "owned runtime task join failed during shutdown");
            if teardown_error.is_none() {
                teardown_error = Some(anyhow!("{name} task join failed during shutdown: {error}"));
            }
        }
    }
    if let Some(handle) = runtime_snapshot_observer_handle {
        if let Err(error) = handle.await {
            tracing::error!(error = %error, "runtime snapshot observer join failed during shutdown");
            if teardown_error.is_none() {
                teardown_error = Some(anyhow!(
                    "runtime snapshot observer join failed during shutdown: {error}"
                ));
            }
        }
    }
    lsp_pool.shutdown().await;

    if let Err(error) = lifecycle.finish_shutdown().await {
        tracing::error!(error = %error, "failed to finish ordered runtime shutdown");
        if teardown_error.is_none() {
            teardown_error = Some(error);
        }
    }

    match (result, teardown_error) {
        (Err(error), Some(teardown_error)) => {
            Err(error.context(format!("runtime teardown also failed: {teardown_error:#}")))
        }
        (Err(error), None) => Err(error),
        (Ok(()), Some(error)) => Err(error),
        (Ok(()), None) => Ok(()),
    }
}

#[cfg(test)]
mod degraded_reason_tests;

#[cfg(test)]
mod startup_slot_failure_policy_tests;

#[cfg(test)]
mod lifecycle_coordinator_tests;
