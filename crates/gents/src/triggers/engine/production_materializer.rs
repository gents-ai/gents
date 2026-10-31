use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::request::lifecycle::{
    task_goal_session_title, task_session_title,
    write_pending_agent_request_with_lineage_workspace_and_conversation_title, ExecutionOrigin,
    TriggerLineage, WorkspaceLineage,
};
use crate::runtime::snapshot::{ActiveRuntimeSnapshot, ResolvedTask};
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
use crate::triggers::engine::{MaterializerHandle, TriggerKind};

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

    fn runtime_actor(&self) -> Result<::identity::Did> {
        let snapshot = self.snapshot_rx.borrow();
        let node = snapshot
            .node
            .as_ref()
            .context("active runtime snapshot has no node ACP actor")?;
        ::identity::Did::new(node.identity.did().to_owned())
            .context("runtime node DID is not ACP-addressable")
    }

    fn resolve_agent(&self, task: &ResolvedTask) -> Result<(String, String, u64, String)> {
        let snapshot = self.snapshot_rx.borrow().clone();
        let agent = snapshot.agent(&task.agent_id).ok_or_else(|| {
            let reason = snapshot
                .unavailable_public_message(&task.agent_id)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("agent {} is not loaded", task.agent_id));
            anyhow!("resolving agent for task {}: {reason}", task.task_id)
        })?;
        let backend_id = agent
            .backend_id
            .as_deref()
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow!(
                    "agent {} has no backend binding; scheduled fires require a backend",
                    agent.agent_id
                )
            })?;
        Ok((
            agent.agent_id.clone(),
            agent.node_did().to_string(),
            agent.deadline_duration.as_secs(),
            backend_id,
        ))
    }
}

pub(crate) async fn recover_workspace_binding_pending_requests(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<usize> {
    let query = format!(
        r#"{{
            AgentRequest(filter: {{
                lifecycle_state: {{ _eq: "{workspace_binding_pending}" }},
                node_did: {{ _eq: "{node}" }}
            }}) {{
                _docID
                request_id
                node_did
                workspace_id
                workspace_owner_node_did
                workspace_authority
                workspace_seal_hash
            }}
        }}"#,
        workspace_binding_pending = RequestLifecycleState::WorkspaceBindingPending.as_str(),
        node = escape_graphql_string(node_did),
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
        let node_did = request
            .node_did
            .as_deref()
            .context("workspace-binding-pending AgentRequest is missing node_did")?;
        let lineage = WorkspaceLineage {
            workspace_owner_node_did: Some(request.workspace_owner_node_did.clone().context(
                "workspace-binding-pending AgentRequest is missing workspace_owner_node_did",
            )?),
            workspace_id: Some(
                request
                    .workspace_id
                    .clone()
                    .context("workspace-binding-pending AgentRequest is missing workspace_id")?,
            ),
            workspace_authority: Some(request.workspace_authority.clone().context(
                "workspace-binding-pending AgentRequest is missing workspace_authority",
            )?),
            workspace_seal_hash: request.workspace_seal_hash.clone(),
        };
        match crate::tools::workspace::materialize_workspace_binding(
            node,
            &request.request_id,
            request_doc_id,
            node_did,
            &lineage,
        )
        .await
        {
            Ok(()) => {
                crate::request::lifecycle::activate_workspace_bound_request(node, request_doc_id)
                    .await?;
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
        delivery: Option<&crate::triggers::engine::durable::PreparedFire>,
        prepared_ids: Option<(&str, &str)>,
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

        let prepared_ids =
            prepared_ids.map(|(request, session)| (request.to_owned(), session.to_owned()));
        let delivery = delivery.cloned();
        let resolved = self.resolve_agent(task);
        let runtime_actor = self.runtime_actor();
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

        Box::pin(async move {
            let (agent_id, node_did, _deadline_secs, _backend_id) = resolved?;
            let runtime_actor = runtime_actor?;
            crate::request::lifecycle::TriggerExecutionContext::parse(trigger_context.as_deref())?;
            let explicit = WorkspaceLineage::from_trigger_context(trigger_context.as_deref())?;
            let mut workspace = explicit;
            crate::tools::workspace::stamp_workspace_lineage(&node, &node_did, &mut workspace)
                .await?;
            workspace.require_authority_if_workspace_id()?;
            let lineage = TriggerLineage {
                trigger_id: trigger_id.clone(),
                trigger_kind: Some(trigger_kind_str),
                source_doc_id,
                correlation,
                trigger_context,
            };
            let workspace_ref = workspace.is_bound().then_some(&workspace);
            let mut duplicate = false;
            let (enqueued, conversation_title) = if let Some(prepared) = &delivery {
                let mut prepared = prepared.clone();
                if prepared.receipt.goal_id.is_some() {
                    let current = crate::goal::load_canonical_goal(
                        &node,
                        &node_did,
                        &prepared.receipt.session_id,
                    )
                    .await?;
                    prepared.receipt.goal_id = Some(crate::goal::task_assignment_goal_id(
                        current.as_ref().map(|goal| goal.goal_id.as_str()),
                        &node_did,
                        &prepared.receipt.session_id,
                    ));
                }
                let fire = &prepared.receipt;
                let title = task_session_title(&task_label);
                // Written under the requester that owns the session
                // (Lean `Enrollment.runtimeRequesterScope`).
                let session_scope = if prepared.target_existing
                    && matches!(trigger_kind, TriggerKind::Event | TriggerKind::Schedule)
                {
                    crate::session::load_session_requester_scope(
                        node.as_ref(),
                        &node_did,
                        &fire.session_id,
                    )
                    .await?
                } else {
                    None
                };
                let retry_key = if super::event_delivery::is_group_fire_key(&durable_fire_key) {
                    &durable_fire_key
                } else {
                    &fire.fire_key
                };
                let mut spec = crate::request::lifecycle::materialize::pending_agent_request_spec(
                    &node_did,
                    &agent_id,
                    &rendered_prompt,
                    execution_origin,
                    lineage,
                    Some(&title),
                    workspace_ref,
                    &fire.request_id,
                    &fire.session_id,
                    Some(retry_key),
                    session_scope.as_deref(),
                    trigger_doc_id.as_deref(),
                )?;
                spec.input.goal_assignment = fire.goal_id.as_ref().map(|goal_id| {
                    gents_protocol::request_input::GoalAssignmentInput {
                        goal_id: goal_id.clone(),
                        root_request_doc_id: None,
                    }
                });
                let create = crate::request::lifecycle::materialize::build_request(spec)?;
                let admission = crate::request::lifecycle::write_trigger_delivery(
                    &node,
                    runtime_actor,
                    &prepared,
                    &create,
                )
                .await?;
                duplicate = admission.duplicate;
                (admission.request, title)
            } else if let Some(objective) = rendered_goal_objective.as_deref() {
                let identity =
                    crate::goal::task_goal_fire_identity(&node_did, &task_id, &durable_fire_key);
                let conversation_title = task_goal_session_title(&task_label, &identity.retry_key);
                let mut spec = crate::request::lifecycle::materialize::pending_agent_request_spec(
                    &node_did,
                    &agent_id,
                    &rendered_prompt,
                    execution_origin,
                    lineage,
                    Some(&conversation_title),
                    workspace_ref,
                    &identity.request_id,
                    &identity.session_id,
                    Some(&identity.retry_key),
                    None,
                    trigger_doc_id.as_deref(),
                )?;
                spec.input.goal_assignment =
                    Some(gents_protocol::request_input::GoalAssignmentInput {
                        goal_id: crate::goal::deterministic_goal_id(
                            &node_did,
                            &identity.session_id,
                        ),
                        root_request_doc_id: None,
                    });
                let create = crate::request::lifecycle::build_signed_request(
                    spec,
                    crate::request::lifecycle::RequestSigner::RegisteredTarget,
                )
                .await?;
                let enqueued = crate::goal::submit_goal_backed_request_local(
                    &node,
                    runtime_actor,
                    &node_did,
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
                let request_id = prepared_ids
                    .as_ref()
                    .map(|(request, _)| request.clone())
                    .unwrap_or_else(|| {
                        if super::event_delivery::is_group_fire_key(&durable_fire_key) {
                            durable_fire_key.clone()
                        } else {
                            uuid::Uuid::new_v4().to_string()
                        }
                    });
                let conversation_title = task_session_title(&task_label);
                let enqueued =
                    write_pending_agent_request_with_lineage_workspace_and_conversation_title(
                        &node,
                        runtime_actor,
                        &node_did,
                        &agent_id,
                        &rendered_prompt,
                        execution_origin,
                        lineage,
                        Some(&conversation_title),
                        workspace_ref,
                        Some(&request_id),
                        None,
                        trigger_doc_id.as_deref(),
                        prepared_ids.as_ref().map(|(_, session)| session.as_str()),
                    )
                    .await?;
                (enqueued, conversation_title)
            };
            if workspace_ref.is_some() {
                crate::tools::workspace::materialize_workspace_binding(
                    &node,
                    &enqueued.request_id,
                    &enqueued.doc_id,
                    &node_did,
                    &workspace,
                )
                .await?;
                crate::request::lifecycle::activate_workspace_bound_request(
                    &node,
                    &enqueued.doc_id,
                )
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
            if duplicate {
                Err(super::MaterializeDuplicate {
                    request_id: enqueued.request_id,
                }
                .into())
            } else {
                Ok(enqueued.request_id)
            }
        })
    }

    fn has_active_runtime_request_for_trigger(
        &self,
        node_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        let node = self.node.clone();
        let escaped_node_did = escape_graphql_string(node_did);
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let request_exclusion_filter = excluded_request_id
            .map(escape_graphql_string)
            .map(|value| format!(r#", request_id: {{ _neq: "{value}" }}"#))
            .unwrap_or_default();
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        Box::pin(async move {
            // The DID scope is load-bearing (#605): the store also holds
            // other agents' requests for the same trigger id. Rows are
            // fetched (not existence-checked) because a claimed/processing
            // row past its persisted deadline must not gate; expiry is
            // evaluated here, not via lexicographic RFC3339 in the filter,
            // and the result is never capped (a pile-up of expired rows
            // must not hide a later live row and let Serial double-fire).
            let query = format!(
                r#"query {{
                    AgentRequest(
                        filter: {{
                            node_did: {{ _eq: "{node_did}" }},
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }}{request_exclusion_filter},
                            lifecycle_state: {{ _in: {active_runtime_states} }}
                        }}
                    ) {{ _docID request_id lifecycle_state deadline }}
                }}"#,
                node_did = escaped_node_did,
                trigger_id = escaped_trigger_id,
                request_exclusion_filter = request_exclusion_filter,
            );
            let resp = graphql_with_transaction_retry(
                &node,
                &query,
                "query for active runtime AgentRequest by trigger",
            )
            .await?;
            let now = chrono::Utc::now();
            let rows = rows::<AgentRequestRow>(&resp, "AgentRequest")?;
            let found = rows.iter().any(|row| row_gates_serial_fire(row, now));
            Ok(found)
        })
    }

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        node_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<usize>> + Send + '_>> {
        let node = self.node.clone();
        let owner = node_did.to_owned();
        let trigger_id = trigger_id.to_owned();
        let excluded = excluded_request_id.map(str::to_owned);
        Box::pin(async move {
            crate::store::config_client::ConfigAccess::transact_local_write(
                &node,
                None,
                "trigger.supersede_requests",
                |txn| {
                    let owner = &owner;
                    let trigger_id = &trigger_id;
                    let excluded = excluded.as_deref();
                    Box::pin(async move {
                        crate::request::lifecycle::materialize::supersede_trigger_requests_in_txn(
                            txn, owner, trigger_id, excluded,
                        )
                        .await
                    })
                },
            )
            .await
        })
    }

    fn recover_admitted_fire(
        &self,
        identity: &gents_protocol::trigger_delivery::FireIdentity,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Option<String>>> + Send + '_>> {
        let identity = identity.clone();
        Box::pin(async move {
            let key = crate::triggers::engine::durable::fire_key(&identity);
            crate::store::config_client::ConfigAccess::transact_local_readonly(
                &self.node, Some(self.runtime_actor()?), "trigger.recover_admitted_fire",
                |txn| { let key = key.clone(); let identity = identity.clone(); Box::pin(async move {
                    let result = txn.execute(&format!(
                        "{{ TriggerFire(filter: {{fire_key: {{_eq: \"{}\"}}}}, limit: 2) {{request_id}} }}",
                        escape_graphql_string(&key),
                    )).await?;
                    let receipts = result["data"]["TriggerFire"].as_array()
                        .context("fire recovery omitted receipt rows")?;
                    anyhow::ensure!(receipts.len() <= 1, "fire receipt is not unique");
                    let Some(receipt) = receipts.first() else { return Ok(None); };
                    let request_id = identity.request_id();
                    anyhow::ensure!(receipt["request_id"].as_str() == Some(request_id.as_str()), "fire receipt request identity mismatch");
                    let result = txn.execute(&format!(
                        "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}, limit: 2) {{_docID}} }}",
                        escape_graphql_string(&identity.owner_did), escape_graphql_string(&request_id),
                    )).await?;
                    anyhow::ensure!(result["data"]["AgentRequest"].as_array().is_some_and(|rows| rows.len() == 1), "fire receipt has no unique admitted request");
                    Ok(Some(request_id))
                }) },
            ).await
        })
    }

    fn recover_goal_task_fire(
        &self,
        task: &ResolvedTask,
        durable_fire_key: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Option<String>>> + Send + '_>> {
        let node = self.node.clone();
        let resolved = self.resolve_agent(task);
        let task_id = task.task_id.clone();
        let durable_fire_key = durable_fire_key.to_string();
        Box::pin(async move {
            let (agent_id, node_did, _, _) = resolved?;
            let identity =
                crate::goal::task_goal_fire_identity(&node_did, &task_id, &durable_fire_key);
            let expected = crate::goal::TaskGoalRequestBinding {
                node_did: node_did.clone(),
                agent_id,
                session_id: identity.session_id,
                request_id: identity.request_id,
                retry_key: identity.retry_key,
            };
            let query = format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{
                    request_id node_did agent_id session_id retry_key
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
        node_did: &str,
        trigger_id: &str,
        durable_fire_key: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        let node = self.node.clone();
        let owner = node_did.to_string();
        let node_did = escape_graphql_string(node_did);
        let trigger_id = escape_graphql_string(trigger_id);
        let durable_fire_key = durable_fire_key.to_owned();
        Box::pin(async move {
            let query = format!(
                r#"query {{
                    AgentRequest(
                        filter: {{
                            node_did: {{ _eq: "{node_did}" }},
                            caused_by_trigger_id: {{ _eq: "{trigger_id}" }}
                        }}
                    ) {{ _docID request_id retry_key }}
                }}"#,
            );
            let response = graphql_with_transaction_retry(
                &node,
                &query,
                "query for materialized event-trigger group",
            )
            .await?;
            Ok(response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["retry_key"].as_str() == Some(durable_fire_key.as_str())
                            || row
                                .get("request_id")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|request_id| {
                                    super::event_delivery::request_matches_fire_key(
                                        &owner,
                                        request_id,
                                        &durable_fire_key,
                                    )
                                })
                    })
                }))
        })
    }
}
