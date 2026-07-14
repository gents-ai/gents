use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{watch, Mutex};
use tokio::time::MissedTickBehavior;

use crate::agent::ProcessLifecycleState;
use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::session::execute_mutation_with_retry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconcilePhase {
    Idle,
    Debouncing,
    Resolving,
    Diffing,
    Applying,
}

impl ReconcilePhase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Debouncing => "debouncing",
            Self::Resolving => "resolving",
            Self::Diffing => "diffing",
            Self::Applying => "applying",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileResult {
    Startup,
    Noop,
    Applied,
    Error,
}

impl ReconcileResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Noop => "noop",
            Self::Applied => "applied",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeStatusRow {
    agent_did: String,
    process_state: String,
    reconcile_phase: String,
    active_generation: i64,
    router_generation: i64,
    default_behavior_id: String,
    runnable_behavior_count: i64,
    unavailable_behavior_count: i64,
    behavior_executor_capacity: i64,
    behavior_executor_queue_depth: i64,
    behavior_executor_status_json: String,
    last_reconcile_result: String,
    last_reconcile_error: String,
    last_reconcile_completed_at: String,
    updated_at: String,
}

impl RuntimeStatusRow {
    fn new(agent_did: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            agent_did,
            process_state: ProcessLifecycleState::Uninitialized.as_str().to_string(),
            reconcile_phase: ReconcilePhase::Idle.as_str().to_string(),
            active_generation: 0,
            router_generation: 0,
            default_behavior_id: String::new(),
            runnable_behavior_count: 0,
            unavailable_behavior_count: 0,
            behavior_executor_capacity: 0,
            behavior_executor_queue_depth: 0,
            behavior_executor_status_json: "{}".to_string(),
            last_reconcile_result: String::new(),
            last_reconcile_error: String::new(),
            last_reconcile_completed_at: String::new(),
            updated_at: now,
        }
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeStatusHandle {
    node: Arc<defra_node::EmbeddedNode>,
    state: Arc<Mutex<RuntimeStatusRow>>,
    /// Startup build-failure demotions (#559), folded into the runnable /
    /// unavailable counts at every publish so a reconcile republish cannot
    /// silently undo them — and `/healthz` degrades instead of reading green.
    startup_demotions: Arc<crate::startup_readiness::StartupDemotions>,
}

impl RuntimeStatusHandle {
    pub(crate) fn new(node: Arc<defra_node::EmbeddedNode>, agent_did: impl Into<String>) -> Self {
        Self {
            node,
            state: Arc::new(Mutex::new(RuntimeStatusRow::new(agent_did.into()))),
            startup_demotions: Arc::new(crate::startup_readiness::StartupDemotions::new()),
        }
    }

    pub(crate) fn startup_demotions(&self) -> Arc<crate::startup_readiness::StartupDemotions> {
        self.startup_demotions.clone()
    }

    /// Re-publish counts after a startup demotion so the degradation is
    /// visible immediately, not only at the next reconcile publish.
    pub(crate) async fn record_startup_demotion(&self) {
        self.update(|row| {
            if row.runnable_behavior_count > 0 {
                row.runnable_behavior_count -= 1;
            }
            row.unavailable_behavior_count += 1;
            true
        })
        .await;
    }

    pub(crate) async fn set_process_state(&self, state: ProcessLifecycleState) {
        let process_state = state.as_str().to_string();
        self.update(|row| {
            if row.process_state == process_state {
                return false;
            }
            row.process_state = process_state;
            true
        })
        .await;
    }

    pub(crate) async fn set_reconcile_phase(&self, phase: ReconcilePhase) {
        let reconcile_phase = phase.as_str().to_string();
        self.update(|row| {
            if row.reconcile_phase == reconcile_phase {
                return false;
            }
            row.reconcile_phase = reconcile_phase;
            true
        })
        .await;
    }

    pub(crate) async fn publish_startup_snapshot(&self, snapshot: &ActiveRuntimeSnapshot) {
        self.publish_snapshot(snapshot, ReconcileResult::Startup)
            .await;
    }

    pub(crate) async fn publish_noop(&self, snapshot: &ActiveRuntimeSnapshot) {
        self.publish_snapshot(snapshot, ReconcileResult::Noop).await;
    }

    pub(crate) async fn publish_applied(&self, snapshot: &ActiveRuntimeSnapshot) {
        self.publish_snapshot(snapshot, ReconcileResult::Applied)
            .await;
    }

    pub(crate) async fn publish_error(&self, error: &str) {
        let error = error.to_string();
        self.update(|row| {
            let mut changed = false;
            if row.reconcile_phase != ReconcilePhase::Idle.as_str() {
                row.reconcile_phase = ReconcilePhase::Idle.as_str().to_string();
                changed = true;
            }
            if row.last_reconcile_result != ReconcileResult::Error.as_str() {
                row.last_reconcile_result = ReconcileResult::Error.as_str().to_string();
                changed = true;
            }
            if row.last_reconcile_error != error {
                row.last_reconcile_error = error;
                changed = true;
            }
            let now = Utc::now().to_rfc3339();
            if row.last_reconcile_completed_at != now {
                row.last_reconcile_completed_at = now;
                changed = true;
            }
            changed
        })
        .await;
    }

    pub(crate) async fn publish_router_generation(&self, generation: u64) {
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        self.update(|row| {
            if row.router_generation == generation {
                return false;
            }
            row.router_generation = generation;
            true
        })
        .await;
    }

    pub(crate) async fn publish_executor_snapshot(&self, snapshot: &ActiveRuntimeSnapshot) {
        let executor_status = executor_status_fields(snapshot);
        self.update(|row| apply_executor_status(row, &executor_status))
            .await;
    }

    async fn publish_snapshot(&self, snapshot: &ActiveRuntimeSnapshot, result: ReconcileResult) {
        let executor_status = executor_status_fields(snapshot);
        // A behavior demoted for startup build failures is still in the
        // snapshot's runnable set (the snapshot is document-derived); fold the
        // ledger in so republishes report it unavailable, not healthy.
        let demoted = self.startup_demotions.snapshot();
        let demoted_runnable = snapshot
            .behaviors
            .keys()
            .filter(|behavior_id| demoted.contains_key(*behavior_id))
            .count();
        self.update(|row| {
            let mut changed = false;
            let generation = i64::try_from(snapshot.generation).unwrap_or(i64::MAX);
            let runnable_behavior_count =
                i64::try_from(snapshot.behaviors.len().saturating_sub(demoted_runnable))
                    .unwrap_or(i64::MAX);
            let unavailable_behavior_count =
                i64::try_from(snapshot.unavailable_behaviors.len() + demoted_runnable)
                    .unwrap_or(i64::MAX);
            if row.reconcile_phase != ReconcilePhase::Idle.as_str() {
                row.reconcile_phase = ReconcilePhase::Idle.as_str().to_string();
                changed = true;
            }
            if row.active_generation != generation {
                row.active_generation = generation;
                changed = true;
            }
            if row.router_generation == 0 && generation > 0 {
                row.router_generation = generation;
                changed = true;
            }
            if row.default_behavior_id != snapshot.default_behavior_id {
                row.default_behavior_id = snapshot.default_behavior_id.clone();
                changed = true;
            }
            if row.runnable_behavior_count != runnable_behavior_count {
                row.runnable_behavior_count = runnable_behavior_count;
                changed = true;
            }
            if row.unavailable_behavior_count != unavailable_behavior_count {
                row.unavailable_behavior_count = unavailable_behavior_count;
                changed = true;
            }
            if apply_executor_status(row, &executor_status) {
                changed = true;
            }
            if row.last_reconcile_result != result.as_str() {
                row.last_reconcile_result = result.as_str().to_string();
                changed = true;
            }
            if !row.last_reconcile_error.is_empty() {
                row.last_reconcile_error.clear();
                changed = true;
            }
            let now = Utc::now().to_rfc3339();
            if row.last_reconcile_completed_at != now {
                row.last_reconcile_completed_at = now;
                changed = true;
            }
            changed
        })
        .await;
    }

    async fn update<F>(&self, mutate: F)
    where
        F: FnOnce(&mut RuntimeStatusRow) -> bool,
    {
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        if !mutate(&mut next) {
            return;
        }
        next.updated_at = Utc::now().to_rfc3339();
        *guard = next.clone();
        if let Err(error) = upsert_runtime_status(self.node.as_ref(), &next).await {
            tracing::warn!(
                agent_did = %next.agent_did,
                error = %error,
                "failed to persist AgentRuntime status"
            );
        }
    }
}

async fn upsert_runtime_status(
    node: &defra_node::EmbeddedNode,
    row: &RuntimeStatusRow,
) -> anyhow::Result<()> {
    let mutation = format!(
        r#"mutation {{
            upsert_AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                add: {{
                    agent_did: "{agent_did}",
                    process_state: "{process_state}",
                    reconcile_phase: "{reconcile_phase}",
                    active_generation: {active_generation},
                    router_generation: {router_generation},
                    default_behavior_id: "{default_behavior_id}",
                    runnable_behavior_count: {runnable_behavior_count},
                    unavailable_behavior_count: {unavailable_behavior_count},
                    behavior_executor_capacity: {behavior_executor_capacity},
                    behavior_executor_queue_depth: {behavior_executor_queue_depth},
                    behavior_executor_status_json: "{behavior_executor_status_json}",
                    last_reconcile_result: "{last_reconcile_result}",
                    last_reconcile_error: "{last_reconcile_error}",
                    last_reconcile_completed_at: "{last_reconcile_completed_at}",
                    updated_at: "{updated_at}"
                }},
                update: {{
                    process_state: "{process_state}",
                    reconcile_phase: "{reconcile_phase}",
                    active_generation: {active_generation},
                    router_generation: {router_generation},
                    default_behavior_id: "{default_behavior_id}",
                    runnable_behavior_count: {runnable_behavior_count},
                    unavailable_behavior_count: {unavailable_behavior_count},
                    behavior_executor_capacity: {behavior_executor_capacity},
                    behavior_executor_queue_depth: {behavior_executor_queue_depth},
                    behavior_executor_status_json: "{behavior_executor_status_json}",
                    last_reconcile_result: "{last_reconcile_result}",
                    last_reconcile_error: "{last_reconcile_error}",
                    last_reconcile_completed_at: "{last_reconcile_completed_at}",
                    updated_at: "{updated_at}"
                }}
            ) {{ _docID }}
        }}"#,
        agent_did = escape_graphql_string(&row.agent_did),
        process_state = escape_graphql_string(&row.process_state),
        reconcile_phase = escape_graphql_string(&row.reconcile_phase),
        active_generation = row.active_generation,
        router_generation = row.router_generation,
        default_behavior_id = escape_graphql_string(&row.default_behavior_id),
        runnable_behavior_count = row.runnable_behavior_count,
        unavailable_behavior_count = row.unavailable_behavior_count,
        behavior_executor_capacity = row.behavior_executor_capacity,
        behavior_executor_queue_depth = row.behavior_executor_queue_depth,
        behavior_executor_status_json = escape_graphql_string(&row.behavior_executor_status_json),
        last_reconcile_result = escape_graphql_string(&row.last_reconcile_result),
        last_reconcile_error = escape_graphql_string(&row.last_reconcile_error),
        last_reconcile_completed_at = escape_graphql_string(&row.last_reconcile_completed_at),
        updated_at = escape_graphql_string(&row.updated_at),
    );
    let response = execute_mutation_with_retry(node, &mutation, "upsert_runtime_status").await?;
    if response.has_errors() {
        anyhow::bail!("upsert AgentRuntime failed: {:?}", response.errors);
    }
    Ok(())
}

struct ExecutorStatusFields {
    capacity: i64,
    queue_depth: i64,
    status_json: String,
}

fn executor_status_fields(snapshot: &ActiveRuntimeSnapshot) -> ExecutorStatusFields {
    let statuses = snapshot.behavior_executor_statuses();
    let capacity = statuses
        .values()
        .map(|status| status.worker_capacity)
        .sum::<usize>();
    let queue_depth = statuses
        .values()
        .map(|status| status.queue_depth)
        .sum::<usize>();
    let status_json = serde_json::to_string(&statuses).unwrap_or_else(|_| "{}".to_string());

    ExecutorStatusFields {
        capacity: i64::try_from(capacity).unwrap_or(i64::MAX),
        queue_depth: i64::try_from(queue_depth).unwrap_or(i64::MAX),
        status_json,
    }
}

fn apply_executor_status(row: &mut RuntimeStatusRow, status: &ExecutorStatusFields) -> bool {
    let mut changed = false;
    if row.behavior_executor_capacity != status.capacity {
        row.behavior_executor_capacity = status.capacity;
        changed = true;
    }
    if row.behavior_executor_queue_depth != status.queue_depth {
        row.behavior_executor_queue_depth = status.queue_depth;
        changed = true;
    }
    if row.behavior_executor_status_json != status.status_json {
        row.behavior_executor_status_json = status.status_json.clone();
        changed = true;
    }
    changed
}

pub(crate) async fn run_executor_status_observer(
    mut active_snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    runtime_status: RuntimeStatusHandle,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }

        let snapshot = active_snapshot_rx.borrow().clone();
        runtime_status
            .publish_executor_snapshot(snapshot.as_ref())
            .await;

        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            changed = active_snapshot_rx.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
            _ = interval.tick() => {}
        }
    }
}

#[cfg(test)]
mod tests;
