//! Production `MaterializerHandle` used by the `TriggerEngine` at runtime.
//!
//! Bridges the engine's trigger-neutral materialize/concurrency API to the
//! concrete lifecycle + DefraDB surface:
//!
//! * `materialize` calls `RequestLifecycle::materialize_claimed_with_execution_binding`
//!   with a populated `TriggerLineage` so the persisted `AgentRequest` records
//!   `caused_by_trigger_id` / `caused_by_trigger_kind`.
//! * `has_nonterminal_request_for_trigger` performs a GraphQL query against
//!   `AgentRequest`, filtering on the `(trigger_id, trigger_kind)` tuple and
//!   the non-terminal lifecycle states (`pending`, `claimed`, `processing`,
//!   `inputRequired`).
//! * `supersede_nonterminal_requests_for_trigger` transitions every matching
//!   non-terminal request to `lifecycle_state = superseded` /
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
use crate::lifecycle::{ExecutionOrigin, RequestLifecycle, TriggerLineage};
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

    /// Resolve the `BehaviorConfig` for the materialized task against the
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
                    behavior.name
                )
            })?;
        Ok((
            behavior.name.clone(),
            behavior.did().to_string(),
            behavior.deadline_duration.as_secs(),
            backend_id,
        ))
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
        // Resolve behavior synchronously against the current snapshot so any
        // lookup error surfaces before we start allocating owned state for the
        // async body. `materialize_claimed_with_execution_binding` consumes
        // owned strings, so clone anything we need across the await boundary.
        let resolved = self.resolve_behavior(task);
        let node = self.node.clone();
        let task_id = task.task_id.clone();
        let rendered_prompt = rendered_prompt.to_string();
        let trigger_id = trigger_id.map(str::to_owned);
        let trigger_kind_str = trigger_kind.as_str().to_owned();

        Box::pin(async move {
            let (behavior_name, behavior_did, deadline_secs, backend_id) = resolved?;
            let lineage = TriggerLineage {
                trigger_id: trigger_id.clone(),
                trigger_kind: Some(trigger_kind_str),
            };
            let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
                node,
                &behavior_name,
                &behavior_did,
                &rendered_prompt,
                deadline_secs,
                ExecutionOrigin::Scheduled,
                backend_id,
                lineage,
            )
            .await?;
            let request_id = lifecycle.request().request_id.clone();
            tracing::info!(
                task_id = %task_id,
                trigger_id = ?trigger_id,
                request_id = %request_id,
                "materialized AgentRequest for trigger fire"
            );
            Ok(request_id)
        })
    }

    fn has_nonterminal_request_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        let node = self.node.clone();
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let trigger_kind_str = trigger_kind.as_str();
        Box::pin(async move {
            // Strict tuple match on `(caused_by_trigger_id, caused_by_trigger_kind)`
            // + non-terminal `lifecycle_state`. Limit 1 is sufficient: we only
            // need a boolean signal for the concurrency gate.
            let query = format!(
                r#"query {{
                    AgentRequest(
                        filter: {{
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }},
                            caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }},
                            lifecycle_state: {{ _in: ["pending", "claimed", "processing", "inputRequired"] }}
                        }},
                        limit: 1
                    ) {{ _docID }}
                }}"#,
                trigger_id = escaped_trigger_id,
                trigger_kind = trigger_kind_str,
            );
            let resp = node.execute(&query).await;
            if resp.has_errors() {
                anyhow::bail!(
                    "query for nonterminal AgentRequest by trigger failed: {:?}",
                    resp.errors
                );
            }
            let found = resp
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(|rows| rows.as_array())
                .map(|rows| !rows.is_empty())
                .unwrap_or(false);
            Ok(found)
        })
    }

    fn supersede_nonterminal_requests_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<usize>> + Send + '_>> {
        let node = self.node.clone();
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let trigger_kind_str = trigger_kind.as_str();
        Box::pin(async move {
            // Single bulk update against the non-terminal tuple match. DefraDB
            // returns the list of updated documents in the mutation response
            // so we can count how many requests were transitioned — the
            // engine's `LatestOnly` path treats this count as observational
            // (logged, not gated on). Failure reason left blank; the engine
            // layer does not have a structured reason to attach.
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }},
                            caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }},
                            lifecycle_state: {{ _in: ["pending", "claimed", "processing", "inputRequired"] }}
                        }},
                        input: {{
                            status: "superseded",
                            lifecycle_state: "superseded"
                        }}
                    ) {{ _docID }}
                }}"#,
                trigger_id = escaped_trigger_id,
                trigger_kind = trigger_kind_str,
            );
            let resp = node.execute(&mutation).await;
            if resp.has_errors() {
                anyhow::bail!(
                    "supersede nonterminal AgentRequests by trigger failed: {:?}",
                    resp.errors
                );
            }
            let count = resp
                .data
                .as_ref()
                .and_then(|data| data.get("update_AgentRequest"))
                .and_then(|rows| rows.as_array())
                .map(|rows| rows.len())
                .unwrap_or(0);
            if count > 0 {
                tracing::info!(
                    trigger_id = %escaped_trigger_id,
                    trigger_kind = %trigger_kind_str,
                    count,
                    "superseded non-terminal AgentRequests for trigger"
                );
            }
            Ok(count)
        })
    }
}
