//! Production `MaterializerHandle` used by the `TriggerEngine` at runtime.

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

const EXPIRED_CLAIM_GRACE_SECS: i64 = 60;

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

        let resolved = self.resolve_behavior(task);
        let node = self.node.clone();
        let task_id = task.task_id.clone();
        let task_label = task.display_label().to_string();
        let rendered_prompt = rendered_prompt.to_string();
        let trigger_id = trigger_id.map(str::to_owned);
        let trigger_kind_str = trigger_kind.as_str().to_owned();

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
            // id, and those must never gate this agent's fires.
            // terminal-in-effect and must not gate: the owning loop enforces
            // not cap this result: a pile-up of expired orphan rows must not
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
