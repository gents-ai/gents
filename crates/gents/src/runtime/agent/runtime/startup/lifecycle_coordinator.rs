use std::sync::Arc;

use anyhow::Result;
use gents_protocol::node_readiness::{NodeReadinessProcessState, ProcessAction};
use tokio::sync::Mutex;

use crate::runtime::agent::{ProcessLifecycleObserver, ProcessLifecycleState};
use crate::runtime::status::RuntimeStatusHandle;

#[derive(Default)]
struct Lifecycle {
    closing: bool,
    stuck_requests: bool,
    admitted: bool,
    drained: bool,
    store_unwritable: bool,
}

/// Sole owner of the process-state/admission ordering boundary: Ready is
/// persisted before the gate opens; shutdown closes the gate first. Every
/// durable process-state write goes through Lean `ProcessState.next`.
pub(super) struct RuntimeLifecycleCoordinator {
    lifecycle: Mutex<Lifecycle>,
    admission_gate: crate::runtime::agent::runtime::router::RuntimeAdmissionGate,
    runtime_status: RuntimeStatusHandle,
    observer: Option<Arc<dyn ProcessLifecycleObserver>>,
}

impl RuntimeLifecycleCoordinator {
    pub(super) fn new(
        admission_gate: crate::runtime::agent::runtime::router::RuntimeAdmissionGate,
        runtime_status: RuntimeStatusHandle,
        observer: Option<Arc<dyn ProcessLifecycleObserver>>,
    ) -> Self {
        Self {
            lifecycle: Mutex::new(Lifecycle::default()),
            admission_gate,
            runtime_status,
            observer,
        }
    }

    /// Startup recovery has run; when it repaired work a predecessor left, or
    /// could not tell, the process is durably Recovering until it is ready.
    pub(super) async fn startup_recovered(&self, stuck_requests: bool) -> Result<()> {
        let mut lifecycle = self.lifecycle.lock().await;
        lifecycle.stuck_requests = stuck_requests;
        if stuck_requests {
            self.apply(&lifecycle, ProcessAction::StartupRecover)
                .await?;
            self.notify(ProcessLifecycleState::Recovering);
        }
        Ok(())
    }

    pub(super) async fn become_ready(&self) -> Result<bool> {
        let mut lifecycle = self.lifecycle.lock().await;
        if lifecycle.closing {
            return Ok(false);
        }
        let action = if lifecycle.stuck_requests {
            ProcessAction::RecoveryComplete
        } else {
            ProcessAction::StartupClean
        };
        self.apply(&lifecycle, action).await?;
        self.admission_gate.open().await;
        lifecycle.admitted = true;
        self.notify(ProcessLifecycleState::Ready);
        Ok(true)
    }

    pub(super) async fn begin_shutdown(&self) -> Result<()> {
        let mut lifecycle = self.lifecycle.lock().await;
        if lifecycle.closing {
            return Ok(());
        }
        self.close(&mut lifecycle).await
    }

    /// The ordered teardown joined every task that runs requests, so none this
    /// process admitted is still active.
    pub(super) async fn finish_shutdown(&self) -> Result<()> {
        let mut lifecycle = self.lifecycle.lock().await;
        lifecycle.drained = true;
        self.finish(&mut lifecycle).await
    }

    /// The run is over, by an ordered teardown, a failed startup or a teardown
    /// its deadline cut. Shutdown is written only when no admitted request can
    /// still be active; otherwise the durable state stays ShuttingDown and the
    /// next start recovers.
    pub(super) async fn finish_run(&self) -> Result<()> {
        let mut lifecycle = self.lifecycle.lock().await;
        let mut failure = None;
        if !lifecycle.closing {
            if let Err(error) = self.close(&mut lifecycle).await {
                tracing::error!(error = %error, "failed to publish terminal ShuttingDown state");
                failure = Some(error);
            }
        }
        if lifecycle.admitted && !lifecycle.drained {
            tracing::warn!(
                "runtime stopped before its admitted requests drained; Shutdown is not written and the next start recovers them"
            );
        } else if let Err(error) = self.finish(&mut lifecycle).await {
            tracing::error!(error = %error, "failed to publish terminal Shutdown state");
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }

    async fn close(&self, lifecycle: &mut Lifecycle) -> Result<()> {
        lifecycle.closing = true;
        self.admission_gate.close().await;
        self.apply_tolerating_unwritable_store(lifecycle, ProcessAction::BeginShutdown)
            .await?;
        self.notify(ProcessLifecycleState::ShuttingDown);
        Ok(())
    }

    async fn finish(&self, lifecycle: &mut Lifecycle) -> Result<()> {
        if self
            .runtime_status
            .readiness()
            .observation()
            .process_state()
            == NodeReadinessProcessState::Shutdown
        {
            return Ok(());
        }
        if lifecycle.store_unwritable {
            tracing::warn!(
                "runtime storage is unavailable; the durable process state stays where the last persisted write left it"
            );
            return Ok(());
        }
        self.apply_tolerating_unwritable_store(lifecycle, ProcessAction::FinishShutdown)
            .await?;
        self.notify(ProcessLifecycleState::Shutdown);
        Ok(())
    }

    async fn apply_tolerating_unwritable_store(
        &self,
        lifecycle: &mut Lifecycle,
        action: ProcessAction,
    ) -> Result<()> {
        match self.apply(lifecycle, action).await {
            Err(error) if crate::runtime::storage_fault::is_unwritable_store(&error) => {
                lifecycle.store_unwritable = true;
                crate::runtime::storage_fault::tolerate_unwritable_store(Err(error))
            }
            other => other.map(|_| ()),
        }
    }

    async fn apply(
        &self,
        lifecycle: &Lifecycle,
        action: ProcessAction,
    ) -> Result<NodeReadinessProcessState> {
        let active_is_zero = !lifecycle.admitted || lifecycle.drained;
        match self
            .runtime_status
            .apply_process_action(action, lifecycle.stuck_requests, active_is_zero)
            .await?
        {
            Ok(written) => Ok(written),
            Err(refused) => {
                tracing::error!(
                    from = refused.from.as_str(),
                    action = ?refused.action,
                    "the process lifecycle refused a durable state write"
                );
                Err(refused.into())
            }
        }
    }

    fn notify(&self, state: ProcessLifecycleState) {
        if let Some(observer) = &self.observer {
            observer.on_process_state_change(state);
        }
    }
}
