use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Mutex;

use crate::agent::ProcessLifecycleState;
use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;

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
}

impl RuntimeStatusHandle {
    pub(crate) fn new(node: Arc<defra_node::EmbeddedNode>, agent_did: impl Into<String>) -> Self {
        Self {
            node,
            state: Arc::new(Mutex::new(RuntimeStatusRow::new(agent_did.into()))),
        }
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

    async fn publish_snapshot(&self, snapshot: &ActiveRuntimeSnapshot, result: ReconcileResult) {
        self.update(|row| {
            let mut changed = false;
            let generation = i64::try_from(snapshot.generation).unwrap_or(i64::MAX);
            let runnable_behavior_count =
                i64::try_from(snapshot.behaviors.len()).unwrap_or(i64::MAX);
            let unavailable_behavior_count =
                i64::try_from(snapshot.unavailable_behaviors.len()).unwrap_or(i64::MAX);
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
        last_reconcile_result = escape_graphql_string(&row.last_reconcile_result),
        last_reconcile_error = escape_graphql_string(&row.last_reconcile_error),
        last_reconcile_completed_at = escape_graphql_string(&row.last_reconcile_completed_at),
        updated_at = escape_graphql_string(&row.updated_at),
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("upsert AgentRuntime failed: {:?}", response.errors);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
