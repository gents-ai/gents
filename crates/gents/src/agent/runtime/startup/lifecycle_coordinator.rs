use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::agent::{ProcessLifecycleObserver, ProcessLifecycleState};
use crate::runtime_status::RuntimeStatusHandle;

/// Sole owner of the process-state/admission ordering boundary: Ready is
/// persisted before the gate opens; shutdown closes the gate first.
pub(super) struct RuntimeLifecycleCoordinator {
    closing: Mutex<bool>,
    admission_gate: crate::agent::runtime::router::RuntimeAdmissionGate,
    runtime_status: RuntimeStatusHandle,
    observer: Option<Arc<dyn ProcessLifecycleObserver>>,
}

impl RuntimeLifecycleCoordinator {
    pub(super) fn new(
        admission_gate: crate::agent::runtime::router::RuntimeAdmissionGate,
        runtime_status: RuntimeStatusHandle,
        observer: Option<Arc<dyn ProcessLifecycleObserver>>,
    ) -> Self {
        Self {
            closing: Mutex::new(false),
            admission_gate,
            runtime_status,
            observer,
        }
    }

    pub(super) async fn become_ready(&self) -> Result<bool> {
        let closing = self.closing.lock().await;
        if *closing {
            return Ok(false);
        }
        self.runtime_status
            .set_process_state_durable(ProcessLifecycleState::Ready)
            .await?;
        self.admission_gate.open().await;
        if let Some(observer) = &self.observer {
            observer.on_process_state_change(ProcessLifecycleState::Ready);
        }
        Ok(true)
    }

    pub(super) async fn begin_shutdown(&self) -> Result<()> {
        let mut closing = self.closing.lock().await;
        if *closing {
            return Ok(());
        }
        *closing = true;
        self.admission_gate.close().await;
        self.runtime_status
            .set_process_state_durable(ProcessLifecycleState::ShuttingDown)
            .await?;
        if let Some(observer) = &self.observer {
            observer.on_process_state_change(ProcessLifecycleState::ShuttingDown);
        }
        Ok(())
    }

    pub(super) async fn finish_shutdown(&self) -> Result<()> {
        let _closing = self.closing.lock().await;
        self.runtime_status
            .set_process_state_durable(ProcessLifecycleState::Shutdown)
            .await?;
        if let Some(observer) = &self.observer {
            observer.on_process_state_change(ProcessLifecycleState::Shutdown);
        }
        Ok(())
    }
}
