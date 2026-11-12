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
use crate::tools::native::lsp::LspPool;

/// Whole-teardown bound.
///
/// Owned tasks bound their own work, but nothing bounded the drain of all of
/// them, so one task that never observes cancellation could hold the process
/// open indefinitely. The process holds the home's store lock and startup
/// reservation until it exits, and a restart is the only recovery from a
/// latched store, so teardown that cannot finish must still end and let the
/// replacement take the home.
///
/// It is derived from the readiness publisher's own command bound, the one
/// bound that can legitimately still be running inside this drain: anything
/// shorter would truncate work that is already bounded, and nothing here
/// justifies waiting longer than that plus a margin for the rest of the drain.
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
    let task_hook_recoveries = tokio::spawn(async move { task_hook_records.shutdown().await });
    let drain = async {
        let mut drain_error = None;
        while let Some(joined) = background_tasks.join_next().await {
            let task_result = match joined {
                Ok(task) => task.into_result(),
                Err(error) => Err(anyhow!(
                    "background task join failed during shutdown: {error}"
                )),
            };
            if let Err(error) = task_result {
                record_teardown_failure(
                    &mut drain_error,
                    error,
                    "background task exited during shutdown",
                );
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
                record_teardown_failure(
                    &mut drain_error,
                    anyhow!("{name} task join failed during shutdown: {error}"),
                    "owned runtime task join failed during shutdown",
                );
            }
        }
        if let Some(handle) = runtime_snapshot_observer_handle {
            if let Err(error) = handle.await {
                record_teardown_failure(
                    &mut drain_error,
                    anyhow!("runtime snapshot observer join failed during shutdown: {error}"),
                    "runtime snapshot observer join failed during shutdown",
                );
            }
        }
        lsp_pool.shutdown().await;
        drain_error
    };

    // Dropping the drain aborts the handles it still holds, which is what
    // releases the process for the replacement runtime.
    match tokio::time::timeout(TEARDOWN_DEADLINE, drain).await {
        Ok(drain_error) => {
            if teardown_error.is_none() {
                teardown_error = drain_error;
            }
        }
        Err(_) => {
            record_teardown_failure(
                &mut teardown_error,
                anyhow!(
                    "runtime teardown exceeded {}s; remaining owned tasks were aborted",
                    TEARDOWN_DEADLINE.as_secs()
                ),
                "runtime teardown deadline reached",
            );
        }
    }

    if let Err(error) = lifecycle.finish_shutdown().await {
        record_teardown_failure(
            &mut teardown_error,
            error,
            "failed to finish ordered runtime shutdown",
        );
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

/// A store that cannot accept writes is the incident being shut down, not a
/// teardown defect: the runtime already retained it as a process-local storage
/// fault, and making it the teardown error would hide whatever else failed and
/// report a failed stop for an expected one.
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
