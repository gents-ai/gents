use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use tokio::sync::{watch, Mutex};
use tokio::time::MissedTickBehavior;

use crate::agent::ProcessLifecycleState;
use crate::behavior_readiness_publisher::{
    BehaviorReadinessPublisherHandle, BehaviorReadinessPublisherOwner,
};
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
    reconcile_phase: String,
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
            reconcile_phase: ReconcilePhase::Idle.as_str().to_string(),
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

pub(crate) struct RuntimeStatusOwner {
    readiness: BehaviorReadinessPublisherOwner,
}

impl RuntimeStatusOwner {
    pub(crate) async fn close(self) -> anyhow::Result<()> {
        self.readiness.close().await
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeStatusHandle {
    node: Arc<defra_node::EmbeddedNode>,
    state: Arc<Mutex<RuntimeStatusRow>>,
    readiness: BehaviorReadinessPublisherHandle,
}

impl RuntimeStatusHandle {
    pub(crate) fn start(
        node: Arc<defra_node::EmbeddedNode>,
        agent_did: impl Into<String>,
    ) -> (RuntimeStatusOwner, Self) {
        let agent_did = agent_did.into();
        let (readiness_owner, readiness) =
            BehaviorReadinessPublisherHandle::start(node.clone(), agent_did.clone());
        (
            RuntimeStatusOwner {
                readiness: readiness_owner,
            },
            Self {
                node,
                state: Arc::new(Mutex::new(RuntimeStatusRow::new(agent_did))),
                readiness,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_readiness_writer(
        node: Arc<defra_node::EmbeddedNode>,
        agent_did: impl Into<String>,
        writer: Arc<dyn crate::behavior_readiness_publisher::BehaviorReadinessWriter>,
        retry_delay: Duration,
    ) -> (RuntimeStatusOwner, Self) {
        let agent_did = agent_did.into();
        let (readiness_owner, readiness) = BehaviorReadinessPublisherHandle::start_with_writer(
            writer,
            agent_did.clone(),
            retry_delay,
        );
        (
            RuntimeStatusOwner {
                readiness: readiness_owner,
            },
            Self {
                node,
                state: Arc::new(Mutex::new(RuntimeStatusRow::new(agent_did))),
                readiness,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn new(node: Arc<defra_node::EmbeddedNode>, agent_did: impl Into<String>) -> Self {
        let agent_did = agent_did.into();
        let (_owner, readiness) = BehaviorReadinessPublisherHandle::start_with_unbounded_test_clock(
            node.clone(),
            agent_did.clone(),
        );
        let handle = Self {
            node,
            state: Arc::new(Mutex::new(RuntimeStatusRow::new(agent_did))),
            readiness,
        };
        handle
    }

    #[cfg(test)]
    pub(crate) fn start_with_unbounded_test_clock(
        node: Arc<defra_node::EmbeddedNode>,
        agent_did: impl Into<String>,
    ) -> (RuntimeStatusOwner, Self) {
        let agent_did = agent_did.into();
        let (readiness_owner, readiness) =
            BehaviorReadinessPublisherHandle::start_with_unbounded_test_clock(
                node.clone(),
                agent_did.clone(),
            );
        (
            RuntimeStatusOwner {
                readiness: readiness_owner,
            },
            Self {
                node,
                state: Arc::new(Mutex::new(RuntimeStatusRow::new(agent_did))),
                readiness,
            },
        )
    }

    pub(crate) fn readiness(&self) -> &BehaviorReadinessPublisherHandle {
        &self.readiness
    }

    pub(crate) async fn initialize_startup(&self, default_behavior_id: &str) -> anyhow::Result<()> {
        self.readiness.initialize(default_behavior_id).await?;
        Ok(())
    }

    pub(crate) async fn set_process_state_durable(
        &self,
        state: ProcessLifecycleState,
    ) -> anyhow::Result<()> {
        self.readiness.set_process_state(state).await
    }

    #[cfg(test)]
    pub(crate) async fn set_process_state(&self, state: ProcessLifecycleState) {
        if let Err(error) = self.set_process_state_durable(state).await {
            tracing::error!(?state, error = %error, "behavior readiness publisher stopped");
        }
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

    pub(crate) async fn publish_startup_snapshot(
        &self,
        snapshot: &ActiveRuntimeSnapshot,
    ) -> anyhow::Result<()> {
        self.publish_snapshot(snapshot, ReconcileResult::Startup)
            .await
    }

    pub(crate) async fn publish_noop(&self, snapshot: &ActiveRuntimeSnapshot) {
        if let Err(error) = self.publish_snapshot(snapshot, ReconcileResult::Noop).await {
            tracing::error!(error = %error, "failed to publish runtime behavior readiness source");
        }
    }

    pub(crate) async fn publish_applied(&self, snapshot: &ActiveRuntimeSnapshot) {
        if let Err(error) = self
            .publish_snapshot(snapshot, ReconcileResult::Applied)
            .await
        {
            tracing::error!(error = %error, "failed to publish runtime behavior readiness source");
        }
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

    pub(crate) async fn publish_router_generation(&self, generation: u64) -> anyhow::Result<()> {
        self.readiness
            .set_router_generation(generation)
            .await
            .with_context(|| format!("publish behavior readiness router generation {generation}"))
    }

    pub(crate) async fn publish_executor_snapshot(&self, snapshot: &ActiveRuntimeSnapshot) {
        let executor_status = executor_status_fields(snapshot);
        self.update(|row| apply_executor_status(row, &executor_status))
            .await;
    }

    async fn publish_snapshot(
        &self,
        snapshot: &ActiveRuntimeSnapshot,
        result: ReconcileResult,
    ) -> anyhow::Result<()> {
        self.readiness.publish_snapshot(snapshot).await?;
        let executor_status = executor_status_fields(snapshot);
        self.update(|row| {
            let mut changed = false;
            if row.reconcile_phase != ReconcilePhase::Idle.as_str() {
                row.reconcile_phase = ReconcilePhase::Idle.as_str().to_string();
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
        Ok(())
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
                    reconcile_phase: "{reconcile_phase}",
                    behavior_executor_capacity: {behavior_executor_capacity},
                    behavior_executor_queue_depth: {behavior_executor_queue_depth},
                    behavior_executor_status_json: "{behavior_executor_status_json}",
                    last_reconcile_result: "{last_reconcile_result}",
                    last_reconcile_error: "{last_reconcile_error}",
                    last_reconcile_completed_at: "{last_reconcile_completed_at}",
                    updated_at: "{updated_at}"
                }},
                update: {{
                    reconcile_phase: "{reconcile_phase}",
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
        reconcile_phase = escape_graphql_string(&row.reconcile_phase),
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
