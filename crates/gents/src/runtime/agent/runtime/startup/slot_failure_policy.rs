use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::runtime::agent::runtime::context::StartupBarrier;
use crate::runtime::status::RuntimeStatusHandle;

pub(super) struct StartupSlotFailurePolicy {
    pub(super) barrier: Arc<StartupBarrier>,
    pub(super) runtime_status: RuntimeStatusHandle,
    pub(super) admission_gate: crate::runtime::agent::runtime::router::RuntimeAdmissionGate,
    pub(super) runtime_cancel: CancellationToken,
    pub(super) fatal_error: mpsc::UnboundedSender<anyhow::Error>,
    pub(super) observer:
        Option<Arc<dyn crate::runtime::startup_readiness::StartupBuildFailureObserver>>,
    pub(super) budget: u32,
}

#[async_trait::async_trait]
impl crate::runtime::agent::reconcile::SlotFailurePolicy for StartupSlotFailurePolicy {
    fn build_failure_budget(&self) -> u32 {
        self.budget.max(1)
    }

    fn on_build_failure(&self, agent_id: &str, failure_number: u32, error: &str) {
        if let Some(observer) = &self.observer {
            observer.on_build_failure(agent_id, failure_number, self.build_failure_budget(), error);
        }
    }

    async fn on_slot_created(&self, agent_id: &str, generation: u64) -> Result<()> {
        self.runtime_status
            .readiness()
            .register_slot(agent_id, generation)
            .await
            .with_context(|| {
                format!("register agent readiness slot {agent_id} generation {generation}")
            })?;
        self.barrier.register_agent(agent_id, generation).await;
        Ok(())
    }

    async fn try_demote(&self, agent_id: &str, generation: u64, error: &str) -> Result<bool> {
        if !self.barrier.is_pending(agent_id, generation).await {
            return Ok(false);
        }
        let reason = format!(
            "demoted after {} consecutive startup build failures; last error: {error}",
            self.build_failure_budget()
        );
        let applied = match self
            .runtime_status
            .readiness()
            .demote_slot(agent_id, generation, reason)
            .await
        {
            Ok(applied) => applied,
            Err(error) => {
                self.admission_gate.close().await;
                let rendered = format!("{error:#}");
                if self.fatal_error.send(error).is_err() {
                    tracing::error!(
                        error = %rendered,
                        "runtime coordinator stopped before demotion failure was delivered"
                    );
                }
                self.runtime_cancel.cancel();
                return Err(anyhow!(rendered));
            }
        };
        if !applied {
            return Ok(false);
        }
        self.barrier.mark_agent_demoted(agent_id, generation).await;
        tracing::error!(
            agent_id = %agent_id,
            budget = self.build_failure_budget(),
            error = %error,
            "agent demoted: its completion client failed to build repeatedly; \
             the process will report Ready without it. Fix the agent/backend \
             config, or install a plugin it names that is missing — the runtime \
             re-admits it on its next reconcile with a fresh budget."
        );
        Ok(true)
    }

    async fn on_slot_retired(&self, agent_id: &str, generation: u64, recreated: bool) {
        if let Err(error) = self
            .runtime_status
            .readiness()
            .retire_slot(agent_id, generation)
            .await
        {
            tracing::error!(agent_id, generation, error = %error, "failed to retire agent_config readiness slot");
        }
        self.barrier
            .mark_agent_superseded(agent_id, generation)
            .await;
        let _ = recreated;
    }
}
