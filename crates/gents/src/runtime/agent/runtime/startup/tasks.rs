use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::super::context::{RuntimeContext, StartupBarrier};
use super::lifecycle_coordinator::RuntimeLifecycleCoordinator;
use super::BackgroundTaskResult;
use crate::inference::admission::AdmissionRegistry;
use crate::runtime::agent::p2p_reconcile::{EnrollmentAuthorityHandle, EnrollmentAuthorityOwner};
use crate::runtime::agent::Gents;
use crate::runtime::snapshot::{ActiveRuntimeSnapshot, ResolvedRuntimeSnapshot};
use crate::runtime::status::RuntimeStatusHandle;
use crate::tools::mcp::health::{spawn_health_checker, ServiceHealthMap};
use crate::tools::surface::ToolRuntimeContext;

#[derive(Clone, Copy)]
pub(super) struct StartupTasks<'a> {
    pub(super) agent: &'a Gents,
    pub(super) cancel: &'a CancellationToken,
    pub(super) shutdown: &'a watch::Receiver<bool>,
    pub(super) runtime_status: &'a RuntimeStatusHandle,
    pub(super) active_snapshot_rx: &'a watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    pub(super) startup_barrier: &'a Arc<StartupBarrier>,
}

pub(super) fn runtime_context(
    agent: &Gents,
    tool_runtime: ToolRuntimeContext,
    admission_registry: &AdmissionRegistry,
    startup_barrier: &Arc<StartupBarrier>,
    runtime_status: &RuntimeStatusHandle,
    enrollment_handle: &EnrollmentAuthorityHandle,
) -> RuntimeContext {
    let job_driver = crate::tools::background::job::driver::JobDriver::with_service_admission(
        crate::tools::background::job::driver::JobRuntime {
            node: agent.node.clone(),
            agent_snapshots: Default::default(),
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
    runtime
}

pub(super) fn spawn_health_probes(
    agent: &Gents,
    health_map: &ServiceHealthMap,
    cancel: &CancellationToken,
) -> (
    AbortOnDropHandle<()>,
    AbortOnDropHandle<()>,
    mpsc::Receiver<()>,
) {
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
    let backend_prober =
        AbortOnDropHandle::new(crate::inference::backends::health::spawn_backend_prober(
            agent.node.clone(),
            agent.backend_health.clone(),
            agent.backend_prober_options.clone(),
            backend_health_events_tx,
            cancel.child_token(),
            agent.node_did().to_string(),
        ));
    (health_checker, backend_prober, backend_health_events_rx)
}

pub(super) fn spawn_snapshot_observer(startup: StartupTasks<'_>) -> Option<AbortOnDropHandle<()>> {
    let StartupTasks {
        agent,
        shutdown,
        active_snapshot_rx,
        ..
    } = startup;
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
    runtime_snapshot_observer_handle
}

pub(super) fn spawn_trigger_engine(startup: StartupTasks<'_>) -> AbortOnDropHandle<()> {
    let StartupTasks {
        agent,
        cancel,
        runtime_status,
        active_snapshot_rx,
        startup_barrier,
        ..
    } = startup;
    let trigger_engine_node = agent.node.clone();
    let trigger_engine_schedule_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_event_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_goal_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_materializer_snapshot_rx = active_snapshot_rx.clone();
    let trigger_engine_cancel = cancel.child_token();
    let trigger_engine_startup_barrier = startup_barrier.clone();
    let trigger_engine_node_did = agent.node_did().to_string();
    let trigger_engine_runtime_observer = agent.runtime_snapshot_observer.clone();
    let trigger_engine_runtime_status = runtime_status.clone();
    let trigger_engine_handle = AbortOnDropHandle::new(tokio::spawn(async move {
        tokio::select! {
            _ = trigger_engine_cancel.cancelled() => return,
            _ = trigger_engine_startup_barrier.wait_ready() => {}
        }
        let recovery_identities = trigger_engine_materializer_snapshot_rx
            .borrow()
            .watcher_identities();
        let workspace_recovery = match recovery_identities {
            Ok(identities) => crate::triggers::engine::production_materializer::recover_workspace_binding_pending_requests(
                &trigger_engine_node, &trigger_engine_node_did, &identities,
            ).await,
            Err(error) => Err(error),
        };
        match workspace_recovery {
            Ok(recovered) if recovered > 0 => {
                tracing::info!(recovered, "recovered workspace-binding-pending requests")
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "workspace binding recovery sweep failed"),
        }
        let schedule_source: Box<dyn crate::triggers::engine::TriggerSource> = Box::new(
            crate::triggers::engine::schedule_source::ScheduleSource::new(
                trigger_engine_schedule_snapshot_rx,
                trigger_engine_node.clone(),
                trigger_engine_cancel.clone(),
            ),
        );
        let event_source: Box<dyn crate::triggers::engine::TriggerSource> = Box::new(
            crate::triggers::engine::event_source::EventSource::new(
                trigger_engine_event_snapshot_rx,
                trigger_engine_node.clone(),
                trigger_engine_cancel.clone(),
            )
            .with_runtime_observer(trigger_engine_runtime_observer),
        );
        let goal_source: Box<dyn crate::triggers::engine::TriggerSource> = Box::new(
            crate::triggers::engine::goal_source::GoalSource::new(
                trigger_engine_goal_snapshot_rx,
                trigger_engine_node.clone(),
                trigger_engine_cancel.clone(),
            )
            .with_runtime_status(trigger_engine_runtime_status),
        );
        let sources: Vec<Box<dyn crate::triggers::engine::TriggerSource>> =
            vec![schedule_source, event_source, goal_source];
        crate::triggers::engine::run_trigger_engine(
            trigger_engine_node,
            trigger_engine_materializer_snapshot_rx,
            sources,
            trigger_engine_cancel,
        )
        .await;
    }));
    trigger_engine_handle
}

pub(super) fn spawn_callback_engine(startup: StartupTasks<'_>) -> AbortOnDropHandle<()> {
    let StartupTasks {
        agent,
        cancel,
        active_snapshot_rx,
        startup_barrier,
        ..
    } = startup;
    let callback_node = agent.node.clone();
    let callback_node_did = agent.node_did().to_string();
    let callback_ceiling = agent
        .document_runtime_context()
        .and_then(|context| context.tool_ceiling.root())
        .map(std::path::Path::to_path_buf);
    crate::tools::workspace::install_process_operator_tool_root(callback_ceiling.clone());
    let callback_plugins = agent.plugins().clone();
    let callback_cancel = cancel.child_token();
    let callback_snapshots = active_snapshot_rx.clone();
    let callback_startup_barrier = startup_barrier.clone();
    let callback_engine_handle = AbortOnDropHandle::new(tokio::spawn(async move {
        tokio::select! {
            _ = callback_cancel.cancelled() => return,
            _ = callback_startup_barrier.wait_ready() => {}
        }
        if let Err(error) = crate::triggers::callback::run_callback_engine(
            callback_node,
            callback_node_did,
            callback_ceiling,
            callback_plugins,
            callback_snapshots,
            callback_cancel,
        )
        .await
        {
            tracing::error!(%error, "callback engine exited");
        }
    }));
    callback_engine_handle
}

pub(super) fn spawn_readiness(
    startup: StartupTasks<'_>,
    lifecycle: &Arc<RuntimeLifecycleCoordinator>,
    initial_active_snapshot: &ActiveRuntimeSnapshot,
    readiness_fatal_error: mpsc::UnboundedSender<anyhow::Error>,
) -> AbortOnDropHandle<()> {
    let StartupTasks {
        cancel,
        runtime_status,
        startup_barrier,
        ..
    } = startup;
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
    readiness_handle
}

pub(super) fn spawn_node_tasks(
    startup: StartupTasks<'_>,
    background_tasks: &mut JoinSet<BackgroundTaskResult>,
    runtime: &RuntimeContext,
    enrollment_owner: EnrollmentAuthorityOwner,
    enrollment_handle: &EnrollmentAuthorityHandle,
) {
    let StartupTasks {
        agent,
        cancel,
        active_snapshot_rx,
        ..
    } = startup;
    let completion_node = agent.node.clone();
    let completion_node_did = agent.node_did().to_string();
    let completion_background_executions = agent.background_execution_registry.clone();
    let completion_job_driver = runtime.job_driver.clone();
    let completion_agents =
        crate::runtime::snapshot::ActiveSnapshotAgentIdentities::boxed(active_snapshot_rx.clone());
    let completion_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::BackgroundCompletion(
            crate::runtime::background_completion_observer::run_background_completion_observer(
                completion_node,
                completion_node_did,
                completion_background_executions,
                completion_job_driver,
                completion_agents,
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
            crate::runtime::agent::p2p_reconcile::run_enrollment_reconciler(
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
            crate::runtime::agent::p2p_reconcile::run_pairing_reconciler(
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
            crate::runtime::agent::p2p_reconcile::run_registry_heartbeat(
                registry_node,
                registry_node_did,
                crate::runtime::agent::p2p_reconcile::resolve_network_id(),
                registry_cancel,
            )
            .await,
        )
    });

    let endpoint_node = agent.node.clone();
    let endpoint_identity = agent.runtime_node_arc().identity.clone();
    let endpoint_cancel = cancel.child_token();
    let advertised_graphql_url = agent.advertised_graphql_url.clone();
    background_tasks.spawn(async move {
        BackgroundTaskResult::EndpointHeartbeat(
            crate::runtime::agent::p2p_reconcile::run_endpoint_heartbeat(
                endpoint_node,
                endpoint_identity,
                advertised_graphql_url,
                endpoint_cancel,
            )
            .await,
        )
    });

    let session_command_node = agent.node.clone();
    let session_command_identity = agent.runtime_node_arc().identity.clone();
    let session_command_agents = active_snapshot_rx.clone();
    let session_command_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::SessionCommandReconcile(
            crate::runtime::agent::session_commands::run_session_command_reconciler_with_agents(
                session_command_node,
                session_command_identity,
                Some(session_command_agents),
                session_command_cancel,
            )
            .await,
        )
    });
}

pub(super) fn spawn_request_tasks(
    startup: StartupTasks<'_>,
    background_tasks: &mut JoinSet<BackgroundTaskResult>,
    admission_gate: &super::super::router::RuntimeAdmissionGate,
) {
    let StartupTasks {
        agent,
        cancel,
        shutdown,
        runtime_status,
        active_snapshot_rx,
        ..
    } = startup;
    let control_node = agent.node.clone();
    let control_node_did = agent.node_did().to_owned();
    let control_executions = agent.background_execution_registry.clone();
    let control_agents =
        crate::runtime::snapshot::ActiveSnapshotAgentIdentities::boxed(active_snapshot_rx.clone());
    let control_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::ToolControlReconcile(
            crate::tools::call::control::client::run(
                control_node,
                control_node_did,
                control_executions,
                control_agents,
                control_cancel,
            )
            .await,
        )
    });

    let directory_node = agent.node.clone();
    let directory_source_did = agent.node_did().to_string();
    let directory_cancel = cancel.child_token();
    background_tasks.spawn(async move {
        BackgroundTaskResult::DirectoryProjection(
            crate::runtime::agent::directory_projection::run_directory_projection(
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
            super::super::router::run_router(
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
            crate::runtime::status::run_executor_status_observer(
                executor_status_active_snapshot_rx,
                executor_status_runtime_status,
                executor_status_shutdown,
            )
            .await,
        )
    });
}

pub(super) fn spawn_control_watcher(
    startup: StartupTasks<'_>,
    background_tasks: &mut JoinSet<BackgroundTaskResult>,
    control_subscription: Option<super::super::control_watcher::ControlSubscription>,
    reconcile_tx: &mpsc::Sender<ResolvedRuntimeSnapshot>,
    control_tool_runtime: ToolRuntimeContext,
    backend_health_events_rx: mpsc::Receiver<()>,
) {
    let StartupTasks {
        agent,
        shutdown,
        runtime_status,
        ..
    } = startup;
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
        let control_backend_admission = agent.backend_admission.clone();
        let control_runtime_status = runtime_status.clone();
        let control_shutdown = shutdown.clone();
        background_tasks.spawn(async move {
            BackgroundTaskResult::Control(
                super::super::control_watcher::run_control_watcher(
                    super::super::control_watcher::ControlWatcherInputs {
                        node: control_node,
                        backend_admission: control_backend_admission,
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
}
