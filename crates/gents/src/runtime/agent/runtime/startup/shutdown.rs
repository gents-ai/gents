use anyhow::{anyhow, Result};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::lifecycle_coordinator::RuntimeLifecycleCoordinator;
use super::BackgroundTaskResult;
use crate::runtime::agent::Gents;
use gents_tools::native::lsp::LspPool;

pub(super) struct OwnedRuntimeTasks {
    pub(super) background_tasks: JoinSet<BackgroundTaskResult>,
    pub(super) readiness_handle: AbortOnDropHandle<()>,
    pub(super) trigger_engine_handle: AbortOnDropHandle<()>,
    pub(super) callback_engine_handle: AbortOnDropHandle<()>,
    pub(super) health_checker: AbortOnDropHandle<()>,
    pub(super) backend_prober: AbortOnDropHandle<()>,
    pub(super) runtime_snapshot_observer_handle: Option<AbortOnDropHandle<()>>,
}

pub(super) async fn shutdown_runtime(
    result: Result<()>,
    agent: &Gents,
    lifecycle: &RuntimeLifecycleCoordinator,
    runtime_shutdown_tx: &watch::Sender<bool>,
    cancel: &CancellationToken,
    tasks: OwnedRuntimeTasks,
    lsp_pool: LspPool,
) -> Result<()> {
    let OwnedRuntimeTasks {
        mut background_tasks,
        readiness_handle,
        trigger_engine_handle,
        callback_engine_handle,
        health_checker,
        backend_prober,
        runtime_snapshot_observer_handle,
    } = tasks;
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
    let background_executions = agent.background_execution_registry.clone();
    let background_process_stops = tokio::spawn(async move {
        let report = background_executions.shutdown().await;
        tracing::info!(
            target: "gents::shutdown",
            stage = "managed_process_stop",
            stopped = report.stopped,
            unstopped = report.unstopped,
            "stopped the background processes this runtime owned"
        );
    });
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
        ("background process stop", background_process_stops.await),
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
