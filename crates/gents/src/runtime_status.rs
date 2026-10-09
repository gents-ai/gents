use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use tokio::sync::{watch, Mutex};
use tokio::time::MissedTickBehavior;

use crate::agent::ProcessLifecycleState;
use crate::graphql::escape_graphql_string;
use crate::node_readiness_publisher::{NodeReadinessPublisherHandle, NodeReadinessPublisherOwner};
use crate::runtime_snapshot::ActiveRuntimeSnapshot;

/// Target of the reconcile-phase transition event.
///
/// `NodeRuntime.reconcile_phase` holds only the phase the runtime is in now,
/// and a reconcile leaves the intermediate phases for as long as its debounce
/// and resolve take. The order and duration of those phases is therefore only
/// observable through this event stream, not by reading the document.
pub(crate) const RECONCILE_PHASE_EVENT_TARGET: &str = "gents.runtime.reconcile_phase";

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
pub(crate) enum ReconcileResult {
    Startup,
    Noop,
    Applied,
    Error,
}

impl ReconcileResult {
    pub(crate) fn as_str(self) -> &'static str {
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
    node_did: String,
    reconcile_phase: String,
    agent_executor_capacity: i64,
    agent_executor_queue_depth: i64,
    agent_executor_status_json: String,
    last_reconcile_result: String,
    last_reconcile_error: String,
    last_reconcile_completed_at: String,
    updated_at: String,
}

impl RuntimeStatusRow {
    fn new(node_did: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            node_did,
            reconcile_phase: ReconcilePhase::Idle.as_str().to_string(),
            agent_executor_capacity: 0,
            agent_executor_queue_depth: 0,
            agent_executor_status_json: "{}".to_string(),
            last_reconcile_result: String::new(),
            last_reconcile_error: String::new(),
            last_reconcile_completed_at: String::new(),
            updated_at: now,
        }
    }
}

pub(crate) struct RuntimeStatusOwner {
    readiness: NodeReadinessPublisherOwner,
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
    readiness: NodeReadinessPublisherHandle,
}

impl RuntimeStatusHandle {
    pub(crate) fn start(
        node: Arc<defra_node::EmbeddedNode>,
        node_did: impl Into<String>,
    ) -> (RuntimeStatusOwner, Self) {
        let node_did = node_did.into();
        let (readiness_owner, readiness) =
            NodeReadinessPublisherHandle::start(node.clone(), node_did.clone());
        (
            RuntimeStatusOwner {
                readiness: readiness_owner,
            },
            Self {
                node,
                state: Arc::new(Mutex::new(RuntimeStatusRow::new(node_did))),
                readiness,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_readiness_writer(
        node: Arc<defra_node::EmbeddedNode>,
        node_did: impl Into<String>,
        writer: Arc<dyn crate::node_readiness_publisher::NodeReadinessWriter>,
        retry_delay: Duration,
    ) -> (RuntimeStatusOwner, Self) {
        let node_did = node_did.into();
        let (readiness_owner, readiness) =
            NodeReadinessPublisherHandle::start_with_writer(writer, node_did.clone(), retry_delay);
        (
            RuntimeStatusOwner {
                readiness: readiness_owner,
            },
            Self {
                node,
                state: Arc::new(Mutex::new(RuntimeStatusRow::new(node_did))),
                readiness,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn new(node: Arc<defra_node::EmbeddedNode>, node_did: impl Into<String>) -> Self {
        let node_did = node_did.into();
        let (_owner, readiness) = NodeReadinessPublisherHandle::start_with_unbounded_test_clock(
            node.clone(),
            node_did.clone(),
        );
        let handle = Self {
            node,
            state: Arc::new(Mutex::new(RuntimeStatusRow::new(node_did))),
            readiness,
        };
        handle
    }

    #[cfg(test)]
    pub(crate) fn start_with_unbounded_test_clock(
        node: Arc<defra_node::EmbeddedNode>,
        node_did: impl Into<String>,
    ) -> (RuntimeStatusOwner, Self) {
        let node_did = node_did.into();
        let (readiness_owner, readiness) =
            NodeReadinessPublisherHandle::start_with_unbounded_test_clock(
                node.clone(),
                node_did.clone(),
            );
        (
            RuntimeStatusOwner {
                readiness: readiness_owner,
            },
            Self {
                node,
                state: Arc::new(Mutex::new(RuntimeStatusRow::new(node_did))),
                readiness,
            },
        )
    }

    pub(crate) fn readiness(&self) -> &NodeReadinessPublisherHandle {
        &self.readiness
    }

    pub(crate) async fn initialize_startup(&self, default_agent_id: &str) -> anyhow::Result<()> {
        self.readiness.initialize(default_agent_id).await?;
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
            tracing::error!(?state, error = %error, "agent readiness publisher stopped");
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
            tracing::error!(error = %error, "failed to publish runtime agent readiness source");
        }
    }

    pub(crate) async fn publish_applied(&self, snapshot: &ActiveRuntimeSnapshot) {
        if let Err(error) = self
            .publish_snapshot(snapshot, ReconcileResult::Applied)
            .await
        {
            tracing::error!(error = %error, "failed to publish runtime agent readiness source");
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
            .with_context(|| format!("publish agent readiness router generation {generation}"))
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
        if next.reconcile_phase != guard.reconcile_phase {
            tracing::info!(
                target: RECONCILE_PHASE_EVENT_TARGET,
                node_did = %next.node_did,
                previous_phase = guard.reconcile_phase.as_str(),
                reconcile_phase = next.reconcile_phase.as_str(),
                "runtime reconcile phase changed"
            );
        }
        *guard = next.clone();
        if let Err(error) = upsert_runtime_status(self.node.as_ref(), &next).await {
            tracing::warn!(
                node_did = %next.node_did,
                error = %error,
                "failed to persist NodeRuntime status"
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
            upsert_NodeRuntime(
                filter: {{ node_did: {{ _eq: "{node_did}" }} }},
                add: {{
                    node_did: "{node_did}",
                    reconcile_phase: "{reconcile_phase}",
                    agent_executor_capacity: {agent_executor_capacity},
                    agent_executor_queue_depth: {agent_executor_queue_depth},
                    agent_executor_status_json: "{agent_executor_status_json}",
                    last_reconcile_result: "{last_reconcile_result}",
                    last_reconcile_error: "{last_reconcile_error}",
                    last_reconcile_completed_at: "{last_reconcile_completed_at}",
                    updated_at: "{updated_at}"
                }},
                update: {{
                    reconcile_phase: "{reconcile_phase}",
                    agent_executor_capacity: {agent_executor_capacity},
                    agent_executor_queue_depth: {agent_executor_queue_depth},
                    agent_executor_status_json: "{agent_executor_status_json}",
                    last_reconcile_result: "{last_reconcile_result}",
                    last_reconcile_error: "{last_reconcile_error}",
                    last_reconcile_completed_at: "{last_reconcile_completed_at}",
                    updated_at: "{updated_at}"
                }}
            ) {{ _docID }}
        }}"#,
        node_did = escape_graphql_string(&row.node_did),
        reconcile_phase = escape_graphql_string(&row.reconcile_phase),
        agent_executor_capacity = row.agent_executor_capacity,
        agent_executor_queue_depth = row.agent_executor_queue_depth,
        agent_executor_status_json = escape_graphql_string(&row.agent_executor_status_json),
        last_reconcile_result = escape_graphql_string(&row.last_reconcile_result),
        last_reconcile_error = escape_graphql_string(&row.last_reconcile_error),
        last_reconcile_completed_at = escape_graphql_string(&row.last_reconcile_completed_at),
        updated_at = escape_graphql_string(&row.updated_at),
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "upsert_runtime_status",
        &mutation,
    )
    .await?;
    if response.has_errors() {
        anyhow::bail!("upsert NodeRuntime failed: {:?}", response.errors);
    }
    Ok(())
}

struct ExecutorStatusFields {
    capacity: i64,
    queue_depth: i64,
    status_json: String,
}

fn executor_status_fields(snapshot: &ActiveRuntimeSnapshot) -> ExecutorStatusFields {
    let statuses = snapshot.agent_executor_statuses();
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
    if row.agent_executor_capacity != status.capacity {
        row.agent_executor_capacity = status.capacity;
        changed = true;
    }
    if row.agent_executor_queue_depth != status.queue_depth {
        row.agent_executor_queue_depth = status.queue_depth;
        changed = true;
    }
    if row.agent_executor_status_json != status.status_json {
        row.agent_executor_status_json = status.status_json.clone();
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
