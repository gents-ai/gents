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
//!   active runtime request to `lifecycle_state = superseded`.
//!
//! Behavior lookup happens against a `watch::Receiver<Arc<ActiveRuntimeSnapshot>>`
//! so the materializer always sees the latest resolved snapshot without
//! needing to re-query the DB at fire time.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
use crate::lifecycle::{
    build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title,
    task_goal_conversation_title, task_run_conversation_title,
    write_pending_agent_request_with_lineage_workspace_and_conversation_title, ExecutionOrigin,
    TriggerLineage, WorkspaceLineage,
};
use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ResolvedTask};
use crate::trigger_engine::{MaterializeSkip, MaterializerHandle, TriggerKind};
use crate::watcher::workspace_bound_request_claimable;

pub(crate) struct ProductionMaterializer {
    node: Arc<EmbeddedNode>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    local_deployment_id: Option<String>,
}

impl ProductionMaterializer {
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    ) -> Self {
        Self {
            node,
            snapshot_rx,
            local_deployment_id: None,
        }
    }

    pub(crate) fn with_local_deployment_id(mut self, deployment_id: impl Into<String>) -> Self {
        let deployment_id = deployment_id.into();
        if !deployment_id.trim().is_empty() {
            self.local_deployment_id = Some(deployment_id);
        }
        self
    }

    fn resolve_behavior(&self, task: &ResolvedTask) -> Result<(String, String, u64, String)> {
        let snapshot = self.snapshot_rx.borrow().clone();
        let behavior = snapshot.behavior(&task.behavior_id).ok_or_else(|| {
            let reason = snapshot
                .unavailable_public_message(&task.behavior_id)
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

pub(crate) async fn recover_workspace_binding_pending_requests(
    node: &EmbeddedNode,
    local_deployment_id: &str,
) -> Result<usize> {
    let query = format!(
        r#"{{
            AgentRequest(filter: {{
                lifecycle_state: {{ _eq: "{workspace_binding_pending}" }},
                workspace_owner_deployment_id: {{ _eq: "{deployment_id}" }}
            }}) {{
                _docID
                request_id
                agent_did
                workspace_id
                workspace_authority
                workspace_owner_deployment_id
                workspace_seal_hash
            }}
        }}"#,
        workspace_binding_pending = RequestLifecycleState::WorkspaceBindingPending.as_str(),
        deployment_id = escape_graphql_string(local_deployment_id),
    );
    let response = graphql_with_transaction_retry(
        node,
        &query,
        "load workspace-binding-pending AgentRequest rows",
    )
    .await?;
    let staged = rows::<AgentRequestRow>(&response, "AgentRequest")?;
    let mut recovered = 0;
    for request in staged {
        let request_doc_id = request
            .doc_id
            .as_deref()
            .context("workspace-binding-pending AgentRequest is missing _docID")?;
        let agent_did = request
            .agent_did
            .as_deref()
            .context("workspace-binding-pending AgentRequest is missing agent_did")?;
        let lineage = WorkspaceLineage {
            workspace_id: Some(
                request
                    .workspace_id
                    .clone()
                    .context("workspace-binding-pending AgentRequest is missing workspace_id")?,
            ),
            workspace_authority: Some(request.workspace_authority.clone().context(
                "workspace-binding-pending AgentRequest is missing workspace_authority",
            )?),
            workspace_owner_deployment_id: Some(
                request.workspace_owner_deployment_id.clone().context(
                    "workspace-binding-pending AgentRequest is missing workspace_owner_deployment_id",
                )?,
            ),
            workspace_seal_hash: request.workspace_seal_hash.clone(),
        };
        match crate::workspace::materialize_workspace_binding(
            node,
            &request.request_id,
            request_doc_id,
            agent_did,
            &lineage,
            Some(local_deployment_id),
        )
        .await
        {
            Ok(()) => {
                crate::lifecycle::activate_workspace_bound_request(node, request_doc_id).await?;
                recovered += 1;
            }
            Err(error) => tracing::warn!(
                request_id = %request.request_id,
                request_doc_id = %request_doc_id,
                %error,
                "workspace binding recovery remains pending"
            ),
        }
    }
    Ok(recovered)
}

pub(crate) fn execution_origin_for_trigger_kind(trigger_kind: TriggerKind) -> ExecutionOrigin {
    match trigger_kind {
        TriggerKind::Manual => ExecutionOrigin::Interactive,
        TriggerKind::Schedule | TriggerKind::Event => ExecutionOrigin::Scheduled,
    }
}

const EXPIRED_CLAIM_GRACE_SECS: i64 = 60;

fn row_gates_serial_fire(row: &AgentRequestRow, now: chrono::DateTime<chrono::Utc>) -> bool {
    let state = row.lifecycle_state;
    if !matches!(
        state,
        Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
    ) {
        return true;
    }
    let Some(deadline) = row
        .deadline
        .as_deref()
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
        trigger_doc_id: Option<&str>,
        source_doc_id: Option<&str>,
        correlation: Option<&str>,
        trigger_context: Option<&str>,
        rendered_prompt: &str,
        rendered_goal_objective: Option<&str>,
        durable_fire_key: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        if matches!(trigger_kind, TriggerKind::Manual)
            && (trigger_id.is_some() || trigger_doc_id.is_some())
        {
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
        let rendered_goal_objective = rendered_goal_objective.map(str::to_owned);
        let goal_token_budget = task.goal_token_budget;
        let durable_fire_key = durable_fire_key.to_string();
        let trigger_id = trigger_id.map(str::to_owned);
        let trigger_doc_id = trigger_doc_id.map(str::to_owned);
        let source_doc_id = source_doc_id.map(str::to_owned);
        let correlation = correlation.map(str::to_owned);
        let trigger_context = trigger_context.map(str::to_owned);
        let trigger_kind_str = trigger_kind.as_str().to_owned();

        let execution_origin = execution_origin_for_trigger_kind(trigger_kind);
        let local_deployment_id = self.local_deployment_id.clone();

        Box::pin(async move {
            let (behavior_name, behavior_did, _deadline_secs, _backend_id) = resolved?;
            let parsed_trigger_context =
                crate::lifecycle::TriggerExecutionContext::parse(trigger_context.as_deref())?;
            let requester_did = parsed_trigger_context
                .source_fields
                .get("requester_did")
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(trigger_id) = trigger_id.as_deref() {
                if let Some(reason) = crate::graph_pipeline::graph_materialization_denial(
                    node.as_ref(),
                    trigger_id,
                    correlation.as_deref(),
                )
                .await?
                {
                    return Err(MaterializeSkip { reason }.into());
                }
            }
            let mut workspace = WorkspaceLineage::from_trigger_context(trigger_context.as_deref())?;
            workspace.require_authority_if_workspace_id()?;
            if workspace.owner_deployment_id().is_some()
                && !workspace_bound_request_claimable(
                    local_deployment_id.as_deref(),
                    workspace.workspace_id.as_deref(),
                    workspace.workspace_owner_deployment_id.as_deref(),
                )
            {
                return Err(MaterializeSkip {
                    reason:
                        "workspace-bound request is owned by another deployment; not claimable here"
                            .to_string(),
                }
                .into());
            }
            crate::workspace::stamp_workspace_lineage(node.as_ref(), &mut workspace).await?;
            if workspace.is_bound()
                && !workspace_bound_request_claimable(
                    local_deployment_id.as_deref(),
                    workspace.workspace_id.as_deref(),
                    workspace.workspace_owner_deployment_id.as_deref(),
                )
            {
                return Err(MaterializeSkip {
                    reason:
                        "workspace-bound request is owned by another deployment; not claimable here"
                            .to_string(),
                }
                .into());
            }
            let lineage = TriggerLineage {
                trigger_id: trigger_id.clone(),
                trigger_kind: Some(trigger_kind_str),
                source_doc_id,
                correlation,
                trigger_context,
            };
            let workspace_ref = workspace.is_bound().then_some(&workspace);
            let (enqueued, conversation_title) = if let Some(objective) =
                rendered_goal_objective.as_deref()
            {
                let identity = crate::goal::task_goal_fire_identity(
                    &behavior_did,
                    &task_id,
                    &durable_fire_key,
                );
                let conversation_title =
                    task_goal_conversation_title(&task_label, &identity.retry_key);
                let create = build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
                    &behavior_did,
                    &behavior_name,
                    &rendered_prompt,
                    execution_origin,
                    lineage,
                    Some(&conversation_title),
                    workspace_ref,
                    &identity.request_id,
                    &identity.session_id,
                    Some(&identity.retry_key),
                    requester_did,
                    trigger_doc_id.as_deref(),
                )
                .await?;
                let enqueued = crate::goal::submit_goal_backed_request_local(
                    node.as_ref(),
                    &behavior_did,
                    &identity.session_id,
                    objective,
                    goal_token_budget,
                    &create,
                )
                .await?;
                (enqueued, conversation_title)
            } else {
                anyhow::ensure!(
                    goal_token_budget.is_none(),
                    "goal token budget requires a goal objective template"
                );
                let request_id = uuid::Uuid::new_v4().to_string();
                let conversation_title = task_run_conversation_title(&task_label);
                let enqueued =
                    write_pending_agent_request_with_lineage_workspace_and_conversation_title(
                        node.as_ref(),
                        &behavior_did,
                        &behavior_name,
                        &rendered_prompt,
                        execution_origin,
                        lineage,
                        Some(&conversation_title),
                        workspace_ref,
                        Some(&request_id),
                        requester_did,
                        trigger_doc_id.as_deref(),
                    )
                    .await?;
                (enqueued, conversation_title)
            };
            if workspace_ref.is_some() {
                crate::workspace::materialize_workspace_binding(
                    node.as_ref(),
                    &enqueued.request_id,
                    &enqueued.doc_id,
                    &behavior_did,
                    &workspace,
                    local_deployment_id.as_deref(),
                )
                .await?;
                crate::lifecycle::activate_workspace_bound_request(node.as_ref(), &enqueued.doc_id)
                    .await?;
            }
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
        correlation: Option<&str>,
        excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        let node = self.node.clone();
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let trigger_kind_str = trigger_kind.as_str();
        let correlation_filter = correlation
            .map(escape_graphql_string)
            .map(|value| format!(r#", caused_by_correlation: {{ _eq: "{value}" }}"#))
            .unwrap_or_default();
        let request_exclusion_filter = excluded_request_id
            .map(escape_graphql_string)
            .map(|value| format!(r#", request_id: {{ _neq: "{value}" }}"#))
            .unwrap_or_default();
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
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
                            caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }}{correlation_filter}{request_exclusion_filter},
                            lifecycle_state: {{ _in: {active_runtime_states} }}
                        }}
                    ) {{ _docID request_id lifecycle_state deadline }}
                }}"#,
                agent_did = escaped_agent_did,
                trigger_id = escaped_trigger_id,
                trigger_kind = trigger_kind_str,
                correlation_filter = correlation_filter,
                request_exclusion_filter = request_exclusion_filter,
            );
            let resp = node.execute(&query).await;
            if resp.has_errors() {
                anyhow::bail!(
                    "query for active runtime AgentRequest by trigger failed: {:?}",
                    resp.errors
                );
            }
            let now = chrono::Utc::now();
            let rows = rows::<AgentRequestRow>(&resp, "AgentRequest")?;
            let found = rows.iter().any(|row| row_gates_serial_fire(row, now));
            Ok(found)
        })
    }

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        agent_did: &str,
        trigger_id: &str,
        trigger_kind: TriggerKind,
        correlation: Option<&str>,
        excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<usize>> + Send + '_>> {
        let node = self.node.clone();
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let trigger_kind_str = trigger_kind.as_str();
        let correlation_filter = correlation
            .map(escape_graphql_string)
            .map(|value| format!(r#", caused_by_correlation: {{ _eq: "{value}" }}"#))
            .unwrap_or_default();
        let request_exclusion_filter = excluded_request_id
            .map(escape_graphql_string)
            .map(|value| format!(r#", request_id: {{ _neq: "{value}" }}"#))
            .unwrap_or_default();
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        Box::pin(async move {
            let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{
                            agent_did: {{ _eq: "{agent_did}" }},
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }},
                            caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }}{correlation_filter}{request_exclusion_filter},
                            lifecycle_state: {{ _in: {active_runtime_states} }}
                        }},
                        input: {{
                            lifecycle_state: "superseded",
                            terminalized_at: "{terminalized_at}",
                            terminal_redrive_attempts: 0
                        }}
                    ) {{ _docID }}
                }}"#,
                agent_did = escaped_agent_did,
                trigger_id = escaped_trigger_id,
                trigger_kind = trigger_kind_str,
                correlation_filter = correlation_filter,
                request_exclusion_filter = request_exclusion_filter,
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

    fn recover_goal_task_fire(
        &self,
        task: &ResolvedTask,
        durable_fire_key: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Option<String>>> + Send + '_>> {
        let node = self.node.clone();
        let resolved = self.resolve_behavior(task);
        let task_id = task.task_id.clone();
        let durable_fire_key = durable_fire_key.to_string();
        Box::pin(async move {
            let (behavior_id, agent_did, _, _) = resolved?;
            let identity =
                crate::goal::task_goal_fire_identity(&agent_did, &task_id, &durable_fire_key);
            let expected = crate::goal::TaskGoalRequestBinding {
                agent_did: agent_did.clone(),
                behavior_id,
                session_id: identity.session_id,
                request_id: identity.request_id,
                retry_key: identity.retry_key,
            };
            let query = format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{
                    request_id agent_did behavior_id session_id retry_key
                }} }}"#,
                escape_graphql_string(&expected.request_id),
            );
            let response = graphql_with_transaction_retry(
                node.as_ref(),
                &query,
                "recover deterministic goal Task request",
            )
            .await?;
            let requests = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            anyhow::ensure!(
                requests.len() <= 1,
                "deterministic goal Task request id is not unique"
            );
            let observed = requests
                .first()
                .cloned()
                .map(serde_json::from_value::<crate::goal::TaskGoalRequestBinding>)
                .transpose()
                .context("decoding deterministic goal Task request binding")?;
            let decision =
                crate::goal::decide_task_goal_fire_recovery(&expected, observed.as_ref());
            match decision.disposition {
                crate::goal::TaskGoalFireRecoveryDisposition::Absent => Ok(None),
                crate::goal::TaskGoalFireRecoveryDisposition::Recovered => {
                    Ok(decision.recovered_request_id)
                }
                crate::goal::TaskGoalFireRecoveryDisposition::Conflict => {
                    anyhow::bail!(
                        "deterministic goal Task request identity conflicts with another binding"
                    )
                }
            }
        })
    }

    fn has_materialized_group_request(
        &self,
        agent_did: &str,
        trigger_id: &str,
        trigger_kind: TriggerKind,
        correlation: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        let node = self.node.clone();
        let agent_did = escape_graphql_string(agent_did);
        let trigger_id = escape_graphql_string(trigger_id);
        let trigger_kind = trigger_kind.as_str();
        let correlation = escape_graphql_string(correlation);
        Box::pin(async move {
            let query = format!(
                r#"query {{
                    AgentRequest(
                        filter: {{
                            agent_did: {{ _eq: "{agent_did}" }},
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }},
                            caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }},
                            caused_by_correlation: {{ _eq: "{correlation}" }}
                        }},
                        limit: 1
                    ) {{ _docID }}
                }}"#,
            );
            let response = node.execute(&query).await;
            if response.has_errors() {
                anyhow::bail!(
                    "query for materialized event-trigger group failed: {:?}",
                    response.errors
                );
            }
            Ok(response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|rows| !rows.is_empty()))
        })
    }
}
