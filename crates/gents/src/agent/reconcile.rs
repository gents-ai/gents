use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinSet;
use tracing::Instrument;

use crate::admission::AdmissionRegistry;
use crate::config::AgentBehavior;
use crate::retry::RetryPolicy;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};
use crate::tool_surface::ToolSurface;
use crate::watcher::AgentRequest;

mod diff;
mod slot;

use diff::diff_counts;
pub(in crate::agent) use slot::SlotFailurePolicy;
use slot::{
    behavior_executor_capacity, spawn_slot_with_capacity, spawn_slots, BehaviorSlot,
    BehaviorSlotState,
};
#[cfg(test)]
use slot::{retire_slot, spawn_slot};

pub(super) struct GenerationSupervisor<F> {
    current_snapshot: Arc<ActiveRuntimeSnapshot>,
    active_slots: HashMap<String, BehaviorSlot>,
    admission_registry: AdmissionRegistry,
    retry_policy: RetryPolicy,
    runner: F,
    runtime_status: RuntimeStatusHandle,
    slot_failure_policy: Option<Arc<dyn SlotFailurePolicy>>,
    retiring_slots: JoinSet<()>,
}

struct StagedSlots {
    slots: HashMap<String, BehaviorSlot>,
    failure_policy: Option<Arc<dyn SlotFailurePolicy>>,
}

impl StagedSlots {
    fn new(failure_policy: Option<Arc<dyn SlotFailurePolicy>>) -> Self {
        Self {
            slots: HashMap::new(),
            failure_policy,
        }
    }

    fn insert(&mut self, behavior_id: String, slot: BehaviorSlot) {
        self.slots.insert(behavior_id, slot);
    }

    fn get(&self, behavior_id: &str) -> Option<&BehaviorSlot> {
        self.slots.get(behavior_id)
    }

    fn into_slots(self) -> HashMap<String, BehaviorSlot> {
        self.slots
    }

    async fn abort(self) {
        for (behavior_id, slot) in self.slots {
            let generation = slot.generation;
            let _ = slot.state_tx.send(BehaviorSlotState::Retiring);
            drop(slot.dispatcher);
            if let Err(error) = slot.handle.await {
                if !error.is_cancelled() {
                    tracing::error!(behavior_id, generation, error = %error, "staged behavior slot join failed during rollback");
                }
            }
            if let Some(policy) = &self.failure_policy {
                policy.on_slot_retired(&behavior_id, generation, true).await;
            }
        }
    }
}

impl<F, Fut> GenerationSupervisor<F>
where
    F: Fn(
            Arc<AgentBehavior>,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            u64,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    pub(super) async fn bootstrap(
        resolved_snapshot: ResolvedRuntimeSnapshot,
        admission_registry: AdmissionRegistry,
        retry_policy: RetryPolicy,
        runner: F,
        runtime_status: RuntimeStatusHandle,
        shutdown: watch::Receiver<bool>,
        slot_failure_policy: Option<Arc<dyn SlotFailurePolicy>>,
    ) -> Result<Self> {
        resolved_snapshot.validate_behavior_readiness_source()?;
        if let Some(policy) = &slot_failure_policy {
            for behavior_id in resolved_snapshot.behaviors.keys() {
                policy.on_slot_created(behavior_id, 1).await;
            }
        }
        admission_registry.reconcile(1, &resolved_snapshot.backend_admission_configs);
        let active_slots = spawn_slots(
            &resolved_snapshot,
            1,
            retry_policy.clone(),
            runner.clone(),
            shutdown,
            slot_failure_policy.clone(),
        );
        let dispatchers = active_slots
            .iter()
            .map(|(behavior_id, slot)| (behavior_id.clone(), slot.dispatcher.clone()))
            .collect();
        let executor_capacities = active_slots
            .iter()
            .map(|(behavior_id, slot)| (behavior_id.clone(), slot.executor_capacity))
            .collect();
        let executor_queue_capacities = active_slots
            .iter()
            .map(|(behavior_id, slot)| (behavior_id.clone(), slot.queue_capacity))
            .collect();
        let current_snapshot = Arc::new(resolved_snapshot.activate_with_executor_metadata(
            1,
            dispatchers,
            executor_capacities,
            executor_queue_capacities,
        ));

        Ok(Self {
            current_snapshot,
            active_slots,
            admission_registry,
            retry_policy,
            runner,
            runtime_status,
            slot_failure_policy,
            retiring_slots: JoinSet::new(),
        })
    }

    pub(super) fn current_snapshot(&self) -> Arc<ActiveRuntimeSnapshot> {
        self.current_snapshot.clone()
    }

    pub(super) async fn run(
        mut self,
        active_snapshot_tx: watch::Sender<Arc<ActiveRuntimeSnapshot>>,
        mut proposals_rx: mpsc::Receiver<ResolvedRuntimeSnapshot>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                proposal = proposals_rx.recv() => {
                    let Some(proposal) = proposal else {
                        break;
                    };
                    let current_generation = self.current_snapshot.generation;
                    let next_generation = current_generation + 1;
                    let proposed_behavior_count = proposal.behaviors.len();
                    let proposed_unavailable_behavior_count = proposal.unavailable_behaviors.len();
                    let proposed_default_behavior_id = proposal.default_behavior_id.clone();

                    self.handle_proposal(proposal, &active_snapshot_tx, shutdown.clone())
                        .instrument(tracing::info_span!(
                            "runtime.reconcile",
                            current_generation,
                            next_generation,
                            proposed_behavior_count,
                            proposed_unavailable_behavior_count,
                            proposed_default_behavior_id = %proposed_default_behavior_id,
                        ))
                        .await;
                }
            }
        }

        self.shutdown_slots().await;
        Ok(())
    }

    async fn handle_proposal(
        &mut self,
        proposal: ResolvedRuntimeSnapshot,
        active_snapshot_tx: &watch::Sender<Arc<ActiveRuntimeSnapshot>>,
        shutdown: watch::Receiver<bool>,
    ) {
        self.runtime_status
            .set_reconcile_phase(ReconcilePhase::Diffing)
            .await;
        if proposal.configuration_fingerprint() == self.current_snapshot.configuration_fingerprint()
        {
            tracing::debug!(
                generation = self.current_snapshot.generation,
                "runtime reconcile noop: resolved snapshot matches active generation"
            );
            self.runtime_status
                .publish_noop(self.current_snapshot.as_ref())
                .await;
            return;
        }

        let diff = diff_counts(&self.current_snapshot, &proposal);
        let next_generation = self.current_snapshot.generation + 1;
        self.runtime_status
            .set_reconcile_phase(ReconcilePhase::Applying)
            .await;
        match self
            .apply_snapshot(proposal, next_generation, active_snapshot_tx, shutdown)
            .await
        {
            Ok(()) => {
                tracing::info!(
                    generation = next_generation,
                    added_behaviors = diff.added,
                    removed_behaviors = diff.removed,
                    updated_behaviors = diff.updated,
                    default_changed = diff.default_changed,
                    unavailable_changed = diff.unavailable_changed,
                    "runtime reconcile applied"
                );
                if diff.unavailable_changed {
                    for (behavior_id, reason) in &self.current_snapshot.unavailable_behaviors {
                        tracing::warn!(
                            behavior_id = %behavior_id,
                            public_reason = ?reason.public_reason,
                            diagnostic = %reason.diagnostic,
                            "behavior unavailable after runtime reconcile"
                        );
                    }
                }
                self.runtime_status
                    .publish_applied(self.current_snapshot.as_ref())
                    .await;
            }
            Err(error) => {
                tracing::error!(
                    generation = next_generation,
                    added_behaviors = diff.added,
                    removed_behaviors = diff.removed,
                    updated_behaviors = diff.updated,
                    default_changed = diff.default_changed,
                    unavailable_changed = diff.unavailable_changed,
                    error = %error,
                    "runtime reconcile apply failed; keeping previous active generation"
                );
                self.runtime_status
                    .publish_error(&format!("{error:#}"))
                    .await;
            }
        }
    }

    async fn apply_snapshot(
        &mut self,
        resolved_snapshot: ResolvedRuntimeSnapshot,
        generation: u64,
        active_snapshot_tx: &watch::Sender<Arc<ActiveRuntimeSnapshot>>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        resolved_snapshot.validate_behavior_readiness_source()?;

        // Complete the entire recreation plan before registering generations
        // or spawning executors. Invalid snapshots have no observable side
        // effects on either readiness standing or slot ownership.
        let mut recreated_behavior_ids = Vec::new();
        for (behavior_id, behavior) in &resolved_snapshot.behaviors {
            let tool_surface = resolved_snapshot
                .tool_surfaces
                .get(behavior_id)
                .expect("validated runnable/tool-surface keyset parity");
            let executor_capacity =
                behavior_executor_capacity(behavior, &resolved_snapshot.backend_admission_configs);
            if self
                .active_slots
                .get(behavior_id)
                .is_none_or(|slot| !slot.matches(behavior, tool_surface, executor_capacity))
            {
                recreated_behavior_ids.push(behavior_id.clone());
            }
        }

        if let Some(policy) = &self.slot_failure_policy {
            for behavior_id in &recreated_behavior_ids {
                policy.on_slot_created(behavior_id, generation).await;
            }
        }

        let mut staged = StagedSlots::new(self.slot_failure_policy.clone());
        for behavior_id in &recreated_behavior_ids {
            let behavior = resolved_snapshot
                .behaviors
                .get(behavior_id)
                .expect("recreation plan references a runnable behavior");
            let tool_surface = resolved_snapshot
                .tool_surfaces
                .get(behavior_id)
                .cloned()
                .expect("validated runnable/tool-surface keyset parity");
            let executor_capacity =
                behavior_executor_capacity(behavior, &resolved_snapshot.backend_admission_configs);
            staged.insert(
                behavior_id.clone(),
                spawn_slot_with_capacity(
                    behavior.clone(),
                    tool_surface,
                    executor_capacity,
                    generation,
                    self.retry_policy.clone(),
                    self.runner.clone(),
                    shutdown.clone(),
                    self.slot_failure_policy.clone(),
                ),
            );
        }

        let runnable_behavior_ids = resolved_snapshot
            .behaviors
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut dispatchers = HashMap::new();
        let mut executor_capacities = HashMap::new();
        let mut executor_queue_capacities = HashMap::new();
        for behavior_id in &runnable_behavior_ids {
            let slot = staged
                .get(behavior_id)
                .or_else(|| self.active_slots.get(behavior_id))
                .expect("validated recreation plan owns every runnable slot");
            dispatchers.insert(behavior_id.clone(), slot.dispatcher.clone());
            executor_capacities.insert(behavior_id.clone(), slot.executor_capacity);
            executor_queue_capacities.insert(behavior_id.clone(), slot.queue_capacity);
        }
        let next_snapshot = Arc::new(resolved_snapshot.activate_with_executor_metadata(
            generation,
            dispatchers,
            executor_capacities,
            executor_queue_capacities,
        ));
        // Publish the new source before changing the active dispatcher set.
        // The router observes this generation skew and stops dequeuing until
        // the active snapshot watch catches up, so no request can enter the
        // handoff gap between retiring the old slots and installing the new.
        if let Err(error) = self
            .runtime_status
            .readiness()
            .publish_snapshot(next_snapshot.as_ref())
            .await
        {
            // The candidate snapshot owns dispatcher clones for every staged
            // slot. Drop it before joining rollback so closing the staged
            // owners also closes their request channels.
            drop(next_snapshot);
            staged.abort().await;
            return Err(error).context("publish behavior readiness before active generation");
        }

        let mut next_slots = HashMap::new();
        let mut retired_slots = Vec::new();
        let mut retired_behaviors: Vec<(String, u64, bool)> = Vec::new();
        let mut staged_slots = staged.into_slots();
        for behavior_id in &runnable_behavior_ids {
            if let Some(slot) = staged_slots.remove(behavior_id) {
                if let Some(existing) = self.active_slots.remove(behavior_id) {
                    let old_generation = existing.generation;
                    retired_slots.push(existing);
                    retired_behaviors.push((behavior_id.clone(), old_generation, true));
                }
                next_slots.insert(behavior_id.clone(), slot);
            } else {
                let existing = self
                    .active_slots
                    .remove(behavior_id)
                    .expect("reused slot disappeared before transaction commit");
                next_slots.insert(behavior_id.clone(), existing);
            }
        }
        debug_assert!(staged_slots.is_empty());
        for (behavior_id, slot) in self.active_slots.drain() {
            retired_behaviors.push((behavior_id, slot.generation, false));
            retired_slots.push(slot);
        }

        self.admission_registry
            .reconcile(generation, &next_snapshot.backend_admission_configs);

        self.current_snapshot = next_snapshot.clone();
        self.active_slots = next_slots;
        // Transfer every retiring handle into the supervisor-owned join set
        // before the fallible watch publication. A closed receiver must not
        // detach executors that still own daemons or in-flight requests.
        for slot in retired_slots {
            let _ = slot.state_tx.send(BehaviorSlotState::Retiring);
            drop(slot.dispatcher);
            self.retiring_slots.spawn(async move {
                if let Err(error) = slot.handle.await {
                    if !error.is_cancelled() {
                        tracing::error!(error = %error, "retired behavior slot join failed");
                    }
                }
            });
        }
        active_snapshot_tx
            .send(next_snapshot)
            .map_err(|_| anyhow!("active runtime snapshot receiver closed"))?;

        while let Some(joined) = self.retiring_slots.try_join_next() {
            if let Err(error) = joined {
                if !error.is_cancelled() {
                    tracing::error!(error = %error, "retired behavior slot owner join failed");
                }
            }
        }
        if let Some(policy) = self.slot_failure_policy.clone() {
            for (behavior_id, old_generation, recreated) in retired_behaviors {
                policy
                    .on_slot_retired(&behavior_id, old_generation, recreated)
                    .await;
            }
        }

        Ok(())
    }

    async fn shutdown_slots(mut self) {
        for slot in self.active_slots.into_values() {
            let _ = slot.state_tx.send(BehaviorSlotState::Retiring);
            drop(slot.dispatcher);
            if let Err(error) = slot.handle.await {
                if !error.is_cancelled() {
                    tracing::error!(error = %error, "behavior slot join failed during shutdown");
                }
            }
        }
        while let Some(joined) = self.retiring_slots.join_next().await {
            if let Err(error) = joined {
                if !error.is_cancelled() {
                    tracing::error!(error = %error, "retired behavior slot owner join failed during shutdown");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
