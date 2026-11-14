use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::lifecycle_coordinator::RuntimeLifecycleCoordinator;
use super::BackgroundTaskResult;
use crate::runtime::agent::Gents;
use crate::runtime::node_readiness_publisher::PRODUCTION_COMMAND_TIMEOUT;
use gents_tools::native::lsp::LspPool;

const TEARDOWN_DEADLINE: Duration =
    PRODUCTION_COMMAND_TIMEOUT.saturating_add(Duration::from_secs(5));

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
    let teardown = async move {
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
            record_teardown_failure(
                &mut teardown_error,
                error,
                "failed to begin ordered runtime shutdown",
            );
        }
        runtime_shutdown_tx.send_replace(true);
        cancel.cancel();
        let task_hook_records = agent
            .background_execution_registry
            .task_hook_records()
            .clone();
        let task_hook_recoveries = AbortOnDropHandle::new(tokio::spawn(async move {
            task_hook_records.shutdown().await;
        }));
        let background_executions = agent.background_execution_registry.clone();
        let background_process_stops = AbortOnDropHandle::new(tokio::spawn(async move {
            let report = background_executions.shutdown().await;
            tracing::info!(
                target: "gents::shutdown",
                stage = "managed_process_stop",
                stopped = report.stopped,
                unstopped = report.unstopped,
                "stopped the background processes this runtime owned"
            );
        }));
        while let Some(joined) = background_tasks.join_next().await {
            let task_result = match joined {
                Ok(task) => task.into_result(),
                Err(error) => Err(anyhow!(
                    "background task join failed during shutdown: {error}"
                )),
            };
            if let Err(error) = task_result {
                record_teardown_failure(
                    &mut teardown_error,
                    error,
                    "background task exited during shutdown",
                );
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
                record_teardown_failure(
                    &mut teardown_error,
                    anyhow!("{name} task join failed during shutdown: {error}"),
                    "owned runtime task join failed during shutdown",
                );
            }
        }
        if let Some(handle) = runtime_snapshot_observer_handle {
            if let Err(error) = handle.await {
                record_teardown_failure(
                    &mut teardown_error,
                    anyhow!("runtime snapshot observer join failed during shutdown: {error}"),
                    "runtime snapshot observer join failed during shutdown",
                );
            }
        }
        lsp_pool.shutdown().await;
        if let Err(error) = lifecycle.finish_shutdown().await {
            record_teardown_failure(
                &mut teardown_error,
                error,
                "failed to finish ordered runtime shutdown",
            );
        }
        teardown_error
    };
    let teardown_error = match tokio::time::timeout(TEARDOWN_DEADLINE, teardown).await {
        Ok(error) => error,
        Err(_) => {
            tracing::error!(
                deadline_seconds = TEARDOWN_DEADLINE.as_secs(),
                "runtime teardown deadline reached"
            );
            runtime_shutdown_tx.send_replace(true);
            cancel.cancel();
            Some(anyhow!(
                "runtime teardown exceeded {}s; remaining owned tasks were aborted",
                TEARDOWN_DEADLINE.as_secs()
            ))
        }
    };
    match (result, teardown_error) {
        (Err(error), Some(teardown_error)) => {
            Err(error.context(format!("runtime teardown also failed: {teardown_error:#}")))
        }
        (Err(error), None) => Err(error),
        (Ok(()), Some(error)) => Err(error),
        (Ok(()), None) => Ok(()),
    }
}

fn record_teardown_failure(slot: &mut Option<anyhow::Error>, error: anyhow::Error, message: &str) {
    if crate::runtime::storage_fault::is_unwritable_store(&error) {
        tracing::warn!(error = %error, "{message}: runtime storage is unavailable");
        return;
    }
    tracing::error!(error = %error, "{message}");
    if slot.is_none() {
        *slot = Some(error);
    }
}
