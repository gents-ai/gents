use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::FutureExt;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;

use crate::config::BehaviorConfig;
use crate::retry::RetryPolicy;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};
use crate::tool_surface::ToolSurface;
use crate::watcher::AgentRequest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BehaviorSlotState {
    Active,
    Retiring,
}

struct BehaviorSlot {
    dispatcher: mpsc::Sender<AgentRequest>,
    state_tx: watch::Sender<BehaviorSlotState>,
    handle: JoinHandle<()>,
    behavior_fingerprint: String,
    tool_surface_fingerprint: String,
}

impl BehaviorSlot {
    fn matches(&self, behavior: &Arc<BehaviorConfig>, tool_surface: &Arc<ToolSurface>) -> bool {
        self.behavior_fingerprint == format!("{behavior:?}")
            && self.tool_surface_fingerprint == format!("{tool_surface:?}")
    }
}

pub(super) struct GenerationSupervisor<F> {
    current_snapshot: Arc<ActiveRuntimeSnapshot>,
    active_slots: HashMap<String, BehaviorSlot>,
    retry_policy: RetryPolicy,
    runner: F,
    runtime_status: RuntimeStatusHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotDiffCounts {
    added: usize,
    removed: usize,
    updated: usize,
    default_changed: bool,
    unavailable_changed: bool,
}

impl<F, Fut> GenerationSupervisor<F>
where
    F: Fn(
            Arc<BehaviorConfig>,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    pub(super) fn bootstrap(
        resolved_snapshot: ResolvedRuntimeSnapshot,
        retry_policy: RetryPolicy,
        runner: F,
        runtime_status: RuntimeStatusHandle,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self> {
        let active_slots = spawn_slots(
            &resolved_snapshot,
            retry_policy.clone(),
            runner.clone(),
            shutdown,
        );
        let dispatchers = active_slots
            .iter()
            .map(|(behavior_id, slot)| (behavior_id.clone(), slot.dispatcher.clone()))
            .collect();
        let current_snapshot = Arc::new(resolved_snapshot.activate(1, dispatchers));

        Ok(Self {
            current_snapshot,
            active_slots,
            retry_policy,
            runner,
            runtime_status,
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
                    self.runtime_status
                        .set_reconcile_phase(ReconcilePhase::Diffing)
                        .await;
                    if proposal.configuration_fingerprint() == self.current_snapshot.configuration_fingerprint() {
                        tracing::debug!(
                            generation = self.current_snapshot.generation,
                            "runtime reconcile noop: resolved snapshot matches active generation"
                        );
                        self.runtime_status
                            .publish_noop(self.current_snapshot.as_ref())
                            .await;
                        continue;
                    }
                    let diff = diff_counts(&self.current_snapshot, &proposal);
                    let next_generation = self.current_snapshot.generation + 1;
                    self.runtime_status
                        .set_reconcile_phase(ReconcilePhase::Applying)
                        .await;
                    match self.apply_snapshot(
                        proposal,
                        next_generation,
                        &active_snapshot_tx,
                        shutdown.clone(),
                    ) {
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
                            self.runtime_status.publish_error(&error.to_string()).await;
                        }
                    }
                }
            }
        }

        self.shutdown_slots().await;
        Ok(())
    }

    fn apply_snapshot(
        &mut self,
        resolved_snapshot: ResolvedRuntimeSnapshot,
        generation: u64,
        active_snapshot_tx: &watch::Sender<Arc<ActiveRuntimeSnapshot>>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut next_slots = HashMap::new();
        let mut retired_slots = Vec::new();

        for (behavior_id, behavior) in &resolved_snapshot.behaviors {
            let tool_surface = resolved_snapshot
                .tool_surfaces
                .get(behavior_id)
                .cloned()
                .ok_or_else(|| anyhow!("missing tool surface for behavior {behavior_id}"))?;

            match self.active_slots.remove(behavior_id) {
                Some(existing) if existing.matches(behavior, &tool_surface) => {
                    next_slots.insert(behavior_id.clone(), existing);
                }
                Some(existing) => {
                    retired_slots.push(existing);
                    next_slots.insert(
                        behavior_id.clone(),
                        spawn_slot(
                            behavior.clone(),
                            tool_surface,
                            self.retry_policy.clone(),
                            self.runner.clone(),
                            shutdown.clone(),
                        ),
                    );
                }
                None => {
                    next_slots.insert(
                        behavior_id.clone(),
                        spawn_slot(
                            behavior.clone(),
                            tool_surface,
                            self.retry_policy.clone(),
                            self.runner.clone(),
                            shutdown.clone(),
                        ),
                    );
                }
            }
        }

        retired_slots.extend(self.active_slots.drain().map(|(_, slot)| slot));

        let dispatchers = next_slots
            .iter()
            .map(|(behavior_id, slot)| (behavior_id.clone(), slot.dispatcher.clone()))
            .collect();
        let next_snapshot = Arc::new(resolved_snapshot.activate(generation, dispatchers));

        self.current_snapshot = next_snapshot.clone();
        self.active_slots = next_slots;
        active_snapshot_tx
            .send(next_snapshot)
            .map_err(|_| anyhow!("active runtime snapshot receiver closed"))?;

        for slot in retired_slots {
            retire_slot(slot);
        }

        Ok(())
    }

    async fn shutdown_slots(self) {
        for slot in self.active_slots.into_values() {
            let _ = slot.state_tx.send(BehaviorSlotState::Retiring);
            drop(slot.dispatcher);
            if let Err(error) = slot.handle.await {
                if !error.is_cancelled() {
                    tracing::error!(error = %error, "behavior slot join failed during shutdown");
                }
            }
        }
    }
}

fn spawn_slots<F, Fut>(
    resolved_snapshot: &ResolvedRuntimeSnapshot,
    retry_policy: RetryPolicy,
    runner: F,
    shutdown: watch::Receiver<bool>,
) -> HashMap<String, BehaviorSlot>
where
    F: Fn(
            Arc<BehaviorConfig>,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut slots = HashMap::with_capacity(resolved_snapshot.behaviors.len());
    for (behavior_id, behavior) in &resolved_snapshot.behaviors {
        let tool_surface = resolved_snapshot
            .tool_surfaces
            .get(behavior_id)
            .cloned()
            .expect("resolved snapshot should include tool surfaces for runnable behaviors");
        slots.insert(
            behavior_id.clone(),
            spawn_slot(
                behavior.clone(),
                tool_surface,
                retry_policy.clone(),
                runner.clone(),
                shutdown.clone(),
            ),
        );
    }
    slots
}

fn spawn_slot<F, Fut>(
    behavior: Arc<BehaviorConfig>,
    tool_surface: Arc<ToolSurface>,
    retry_policy: RetryPolicy,
    runner: F,
    shutdown: watch::Receiver<bool>,
) -> BehaviorSlot
where
    F: Fn(
            Arc<BehaviorConfig>,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let (dispatcher, request_rx) = mpsc::channel(32);
    let request_rx = Arc::new(Mutex::new(request_rx));
    let (state_tx, state_rx) = watch::channel(BehaviorSlotState::Active);
    let behavior_fingerprint = format!("{behavior:?}");
    let tool_surface_fingerprint = format!("{tool_surface:?}");

    let handle = tokio::spawn(run_slot_loop(
        behavior,
        tool_surface,
        request_rx,
        retry_policy,
        runner,
        shutdown,
        state_rx,
    ));

    BehaviorSlot {
        dispatcher,
        state_tx,
        handle,
        behavior_fingerprint,
        tool_surface_fingerprint,
    }
}

fn retire_slot(slot: BehaviorSlot) {
    let _ = slot.state_tx.send(BehaviorSlotState::Retiring);
    drop(slot.dispatcher);
    tokio::spawn(async move {
        if let Err(error) = slot.handle.await {
            if !error.is_cancelled() {
                tracing::error!(error = %error, "retired behavior slot join failed");
            }
        }
    });
}

async fn run_slot_loop<F, Fut>(
    behavior: Arc<BehaviorConfig>,
    tool_surface: Arc<ToolSurface>,
    request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
    retry_policy: RetryPolicy,
    runner: F,
    mut shutdown: watch::Receiver<bool>,
    state_rx: watch::Receiver<BehaviorSlotState>,
) where
    F: Fn(
            Arc<BehaviorConfig>,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut failure_count = 0u32;
    loop {
        let outcome = AssertUnwindSafe(runner(
            behavior.clone(),
            tool_surface.clone(),
            request_rx.clone(),
            shutdown.clone(),
        ))
        .catch_unwind()
        .await;

        if *shutdown.borrow() {
            return;
        }

        match outcome {
            Ok(Ok(())) if *state_rx.borrow() == BehaviorSlotState::Retiring => return,
            Ok(Ok(())) => {
                let delay = retry_policy.delay_for_attempt(failure_count);
                failure_count += 1;
                tracing::warn!(
                    behavior_id = %behavior.name,
                    delay_ms = delay.as_millis() as u64,
                    "behavior slot exited unexpectedly, scheduling restart"
                );
                if !wait_for_restart(delay, &mut shutdown).await {
                    return;
                }
            }
            Ok(Err(error)) => {
                let delay = retry_policy.delay_for_attempt(failure_count);
                failure_count += 1;
                tracing::error!(
                    behavior_id = %behavior.name,
                    error = %error,
                    delay_ms = delay.as_millis() as u64,
                    "behavior slot failed, scheduling restart"
                );
                if !wait_for_restart(delay, &mut shutdown).await {
                    return;
                }
            }
            Err(_) => {
                let delay = retry_policy.delay_for_attempt(failure_count);
                failure_count += 1;
                tracing::error!(
                    behavior_id = %behavior.name,
                    delay_ms = delay.as_millis() as u64,
                    "behavior slot panicked, scheduling restart"
                );
                if !wait_for_restart(delay, &mut shutdown).await {
                    return;
                }
            }
        }
    }
}

async fn wait_for_restart(
    delay: std::time::Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => true,
        _ = shutdown.changed() => false,
    }
}

fn diff_counts(
    current: &ActiveRuntimeSnapshot,
    proposed: &ResolvedRuntimeSnapshot,
) -> SnapshotDiffCounts {
    let current_ids = current
        .behaviors
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let proposed_ids = proposed
        .behaviors
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let added = proposed_ids.difference(&current_ids).count();
    let removed = current_ids.difference(&proposed_ids).count();
    let updated = current_ids
        .intersection(&proposed_ids)
        .filter(|behavior_id| {
            let current_behavior = current
                .behaviors
                .get(*behavior_id)
                .expect("intersection must exist in current behaviors");
            let proposed_behavior = proposed
                .behaviors
                .get(*behavior_id)
                .expect("intersection must exist in proposed behaviors");
            format!("{current_behavior:?}") != format!("{proposed_behavior:?}")
                || match (
                    current.tool_surfaces.get(*behavior_id),
                    proposed.tool_surfaces.get(*behavior_id),
                ) {
                    (Some(current_tools), Some(proposed_tools)) => {
                        format!("{current_tools:?}") != format!("{proposed_tools:?}")
                    }
                    _ => true,
                }
        })
        .count();

    SnapshotDiffCounts {
        added,
        removed,
        updated,
        default_changed: current.default_behavior_id != proposed.default_behavior_id,
        unavailable_changed: current.unavailable_behaviors != proposed.unavailable_behaviors,
    }
}

#[cfg(test)]
mod tests;
