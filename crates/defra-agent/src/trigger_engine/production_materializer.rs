//! Production `MaterializerHandle` used by the `TriggerEngine` at runtime.
//!
//! Bridges the engine's trigger-neutral materialize/concurrency API to the
//! concrete lifecycle + DefraDB surface:
//!
//! * `materialize` enqueues a pending `AgentRequest` with populated
//!   `TriggerLineage` so the normal watcher/router/daemon path claims and
//!   executes it while preserving `caused_by_trigger_id` /
//!   `caused_by_trigger_kind`.
//! * `has_active_runtime_request_for_trigger` performs a GraphQL query against
//!   `AgentRequest`, filtering on the `(trigger_id, trigger_kind)` tuple and
//!   the active runtime lifecycle states (`pending`, `claimed`, `processing`).
//! * `supersede_active_runtime_requests_for_trigger` transitions every matching
//!   active runtime request to `lifecycle_state = superseded` /
//!   `status = superseded`.
//!
//! Behavior lookup happens against a `watch::Receiver<Arc<ActiveRuntimeSnapshot>>`
//! so the materializer always sees the latest resolved snapshot without
//! needing to re-query the DB at fire time.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::{
    active_runtime_lifecycle_state_graphql_list, task_run_conversation_title,
    write_pending_agent_request_with_lineage_and_conversation_title, ExecutionOrigin,
    TriggerLineage,
};
use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ResolvedTask};
use crate::trigger_engine::{MaterializerHandle, TriggerKind};

/// Concrete `MaterializerHandle` wired to DefraDB + the lifecycle state
/// machine.
pub(crate) struct ProductionMaterializer {
    node: Arc<EmbeddedNode>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
}

impl ProductionMaterializer {
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    ) -> Self {
        Self { node, snapshot_rx }
    }

    /// Resolve the `AgentBehavior` for the materialized task against the
    /// current active snapshot. Returns an error if the behavior is not
    /// loaded (e.g. unavailable at the time of fire) so the caller can surface
    /// a deterministic `materialize:` failure instead of silently dropping the
    /// fire.
    fn resolve_behavior(&self, task: &ResolvedTask) -> Result<(String, String, u64, String)> {
        let snapshot = self.snapshot_rx.borrow().clone();
        let behavior = snapshot.behavior(&task.behavior_id).ok_or_else(|| {
            let reason = snapshot
                .unavailable_reason(&task.behavior_id)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("behavior {} is not loaded", task.behavior_id));
            anyhow!("resolving behavior for task {}: {reason}", task.task_id)
        })?;
        let backend_id = behavior
            .backend_id
            .as_deref()
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow!(
                    "behavior {} has no backend binding; scheduled fires require a backend",
                    behavior.behavior_id
                )
            })?;
        Ok((
            behavior.behavior_id.clone(),
            behavior.agent_did().to_string(),
            behavior.deadline_duration.as_secs(),
            backend_id,
        ))
    }
}

pub(crate) fn execution_origin_for_trigger_kind(trigger_kind: TriggerKind) -> ExecutionOrigin {
    match trigger_kind {
        TriggerKind::Manual => ExecutionOrigin::Interactive,
        TriggerKind::Schedule | TriggerKind::Event => ExecutionOrigin::Scheduled,
    }
}

/// Grace added to a persisted claim `deadline` before the serial gate treats
/// a claimed/processing row as terminal-in-effect. The owning loop can finish
/// an attempt started just inside the deadline and still be writing the
/// terminal state moments after it; the margin keeps that window from
/// double-firing while still unblocking hours-wedged orphans within a tick
/// or two.
const EXPIRED_CLAIM_GRACE_SECS: i64 = 60;

/// Whether one fetched active-state row actually gates a serial fire at
/// `now`. Pending rows always gate (they are claimable and will run).
/// Claimed/processing rows gate unless their persisted claim `deadline` plus
/// grace has passed — the owning runtime enforces the same deadline
/// in-memory, so a row past it is a wedged orphan, not an in-flight run.
/// Missing or unparseable deadlines gate (conservative).
fn row_gates_serial_fire(row: &serde_json::Value, now: chrono::DateTime<chrono::Utc>) -> bool {
    let state = row
        .get("lifecycle_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if state != "claimed" && state != "processing" {
        return true;
    }
    let Some(deadline) = row
        .get("deadline")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return true;
    };
    match chrono::DateTime::parse_from_rfc3339(deadline) {
        Ok(deadline) => {
            let expired_at = deadline.with_timezone(&chrono::Utc)
                + chrono::Duration::seconds(EXPIRED_CLAIM_GRACE_SECS);
            now <= expired_at
        }
        Err(_) => true,
    }
}

impl MaterializerHandle for ProductionMaterializer {
    fn materialize(
        &self,
        task: &ResolvedTask,
        trigger_id: Option<&str>,
        trigger_kind: TriggerKind,
        rendered_prompt: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        if matches!(trigger_kind, TriggerKind::Manual) && trigger_id.is_some() {
            return Box::pin(async {
                Err(anyhow!(
                    "Manual trigger materialization must not carry trigger_id"
                ))
            });
        }

        // Resolve behavior synchronously against the current snapshot so any
        // lookup error surfaces before we start allocating owned state for the
        // async body. The pending enqueue helper consumes owned strings after
        // the await boundary, so clone anything we need here.
        let resolved = self.resolve_behavior(task);
        let node = self.node.clone();
        let task_id = task.task_id.clone();
        let task_label = task.display_label().to_string();
        let rendered_prompt = rendered_prompt.to_string();
        let trigger_id = trigger_id.map(str::to_owned);
        let trigger_kind_str = trigger_kind.as_str().to_owned();

        // `Manual` fires are operator-initiated (e.g. CLI/API), so they map
        // to `ExecutionOrigin::Interactive` per the spec. Schedule and Event
        // fires keep `Scheduled`. Keep the mapping local to the trait impl so
        // callers (who already decided the `TriggerKind`) don't need to know
        // the lifecycle-layer vocabulary.
        let execution_origin = execution_origin_for_trigger_kind(trigger_kind);

        Box::pin(async move {
            let (behavior_name, behavior_did, _deadline_secs, _backend_id) = resolved?;
            let lineage = TriggerLineage {
                trigger_id: trigger_id.clone(),
                trigger_kind: Some(trigger_kind_str),
            };
            let conversation_title = task_run_conversation_title(&task_label);
            let enqueued = write_pending_agent_request_with_lineage_and_conversation_title(
                node.as_ref(),
                &behavior_did,
                &behavior_name,
                &rendered_prompt,
                execution_origin,
                lineage,
                Some(&conversation_title),
            )
            .await?;
            tracing::info!(
                task_id = %task_id,
                trigger_id = ?trigger_id,
                request_id = %enqueued.request_id,
                session_id = %enqueued.session_id,
                conversation_title = %conversation_title,
                "enqueued AgentRequest for trigger fire"
            );
            Ok(enqueued.request_id)
        })
    }

    fn has_active_runtime_request_for_trigger(
        &self,
        agent_did: &str,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        let node = self.node.clone();
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let trigger_kind_str = trigger_kind.as_str();
        let active_runtime_states = active_runtime_lifecycle_state_graphql_list();
        Box::pin(async move {
            // Strict tuple match on `(agent_did, caused_by_trigger_id,
            // caused_by_trigger_kind)` + active runtime `lifecycle_state`.
            // The DID scope is load-bearing (#605): the replicated store also
            // holds other agents' requests for the same human-chosen trigger
            // id, and those must never gate this agent's fires.
            //
            // Rows are fetched (not just existence-checked) because a claimed
            // or processing row past its persisted claim deadline is
            // terminal-in-effect and must not gate: the owning loop enforces
            // the same deadline in-memory (`await_with_request_deadline`
            // aborts the attempt), so only a wedged orphan — e.g. an owner
            // whose store was rebuilt — can sit past-deadline in an active
            // state, and such a row would otherwise gate forever. Expiry is
            // evaluated here rather than in the filter to avoid relying on
            // lexicographic string comparison over RFC3339 in the store. Do
            // not cap this result: a pile-up of expired orphan rows must not
            // hide a later live row and let Serial double-fire.
            let query = format!(
                r#"query {{
                    AgentRequest(
                        filter: {{
                            agent_did: {{ _eq: "{agent_did}" }},
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }},
                            caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }},
                            lifecycle_state: {{ _in: {active_runtime_states} }}
                        }}
                    ) {{ _docID lifecycle_state deadline }}
                }}"#,
                agent_did = escaped_agent_did,
                trigger_id = escaped_trigger_id,
                trigger_kind = trigger_kind_str,
            );
            let resp = node.execute(&query).await;
            if resp.has_errors() {
                anyhow::bail!(
                    "query for active runtime AgentRequest by trigger failed: {:?}",
                    resp.errors
                );
            }
            let now = chrono::Utc::now();
            let found = resp
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(|rows| rows.as_array())
                .map(|rows| rows.iter().any(|row| row_gates_serial_fire(row, now)))
                .unwrap_or(false);
            Ok(found)
        })
    }

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        agent_did: &str,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<usize>> + Send + '_>> {
        let node = self.node.clone();
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let trigger_kind_str = trigger_kind.as_str();
        let active_runtime_states = active_runtime_lifecycle_state_graphql_list();
        Box::pin(async move {
            let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
            // Single bulk update against the active runtime tuple match,
            // scoped to this agent's own requests (#605): without the DID
            // filter a LatestOnly fire would locally rewrite OTHER agents'
            // replicated requests. Expired claims are deliberately included —
            // superseding this agent's own wedged orphan is desirable.
            // DefraDB returns the list of updated documents in the mutation
            // response so we can count how many requests were transitioned;
            // the engine's `LatestOnly` path treats this count as
            // observational (logged, not gated on). Failure reason left blank;
            // the engine layer does not have a structured reason to attach.
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{
                            agent_did: {{ _eq: "{agent_did}" }},
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }},
                            caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }},
                            lifecycle_state: {{ _in: {active_runtime_states} }}
                        }},
                        input: {{
                            status: "superseded",
                            lifecycle_state: "superseded",
                            terminalized_at: "{terminalized_at}",
                            terminal_redrive_attempts: 0
                        }}
                    ) {{ _docID }}
                }}"#,
                agent_did = escaped_agent_did,
                trigger_id = escaped_trigger_id,
                trigger_kind = trigger_kind_str,
            );
            let resp = crate::retry::execute_graphql_with_terminal_persistence_retry(
                node.as_ref(),
                &mutation,
                "supersede_active_runtime_requests_for_trigger",
            )
            .await?;
            let count = resp
                .data
                .as_ref()
                .and_then(|data| data.get("update_AgentRequest"))
                .and_then(|rows| rows.as_array())
                .map(|rows| rows.len())
                .unwrap_or(0);
            if count > 0 {
                tracing::info!(
                    agent_did = %escaped_agent_did,
                    trigger_id = %escaped_trigger_id,
                    trigger_kind = %trigger_kind_str,
                    count,
                    "superseded active runtime AgentRequests for trigger"
                );
            }
            Ok(count)
        })
    }
}
