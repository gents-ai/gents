//! Canonical automation configuration through the shared candidate transaction.
//! Runtime task invocation below remains a separate integration surface.
use super::super::graphql::{escape_graphql_string, normalize_required};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use gents::collection::Collection;
#[cfg(test)]
use gents::config_client::{apply_desired_state_plan, read_desired_state_record_in_txn};
use gents::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::document_config::{EventSource, Schedule, Task, Trigger};
use gents::{task_session_title, write_manual_agent_request_with_conversation_title};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[cfg(test)]
pub async fn upsert_task(node: &EmbeddedNode, document: &Task) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Task,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan_local(node, "desktop.task.save", plan).await
}

pub async fn upsert_task_on(access: &ConfigAccess, document: &Task) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Task,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.task.save", plan).await
}

#[cfg(test)]
pub async fn delete_task(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.task.delete",
        Collection::Task,
        agent_did,
        id,
    )
    .await
}

pub async fn delete_task_on(access: &ConfigAccess, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.task.delete",
        Collection::Task,
        agent_did,
        id,
    )
    .await
}

#[cfg(test)]
pub async fn upsert_schedule(node: &EmbeddedNode, document: &Schedule) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Schedule,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan_local(node, "desktop.schedule.save", plan).await
}

pub async fn upsert_schedule_on(access: &ConfigAccess, document: &Schedule) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Schedule,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.schedule.save", plan).await
}

#[cfg(test)]
pub async fn delete_schedule(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.schedule.delete",
        Collection::Schedule,
        agent_did,
        id,
    )
    .await
}

pub async fn delete_schedule_on(access: &ConfigAccess, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.schedule.delete",
        Collection::Schedule,
        agent_did,
        id,
    )
    .await
}

#[cfg(test)]
pub async fn upsert_trigger(node: &EmbeddedNode, document: &Trigger) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Trigger,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan_local(node, "desktop.trigger.save", plan).await
}

pub async fn upsert_trigger_on(access: &ConfigAccess, document: &Trigger) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Trigger,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.trigger.save", plan).await
}

#[cfg(test)]
pub async fn delete_trigger(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.trigger.delete",
        Collection::Trigger,
        agent_did,
        id,
    )
    .await
}

pub async fn delete_trigger_on(access: &ConfigAccess, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.trigger.delete",
        Collection::Trigger,
        agent_did,
        id,
    )
    .await
}

#[cfg(test)]
pub async fn upsert_event_source(node: &EmbeddedNode, document: &EventSource) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::EventSource,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan_local(node, "desktop.event_source.save", plan).await
}

pub async fn upsert_event_source_on(access: &ConfigAccess, document: &EventSource) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::EventSource,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.event_source.save", plan).await
}

#[cfg(test)]
pub async fn delete_event_source(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.event_source.delete",
        Collection::EventSource,
        agent_did,
        id,
    )
    .await
}

pub async fn delete_event_source_on(
    access: &ConfigAccess,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.event_source.delete",
        Collection::EventSource,
        agent_did,
        id,
    )
    .await
}

/// Fire a task immediately using the shared manual-run helper.
///
/// Unlike the CLI path (which writes the mutation directly because it may
/// be talking to a remote GraphQL endpoint), desktop is in-process with
/// DefraDB and can call the shared helper directly. Both paths produce
/// the same `(caused_by_trigger_kind = "manual", caused_by_trigger_id =
/// null)` lineage and the same `execution_origin = "interactive"`, so
/// observers can treat them as one origin.
///
/// Returns the new `AgentRequest`'s `_docID` on success.
pub async fn fire_task_now(
    node: &EmbeddedNode,
    actor: identity::Did,
    task_row: &Task,
    args: serde_json::Value,
) -> Result<String> {
    let task_id = normalize_required("task_id", &task_row.task_id)?;
    let agent_did = normalize_required("agent_did", &task_row.agent_did)?;
    let behavior_id = normalize_required("behavior_id", &task_row.behavior_id)?;
    normalize_required("prompt_template", &task_row.prompt_template)?;
    if !task_row.enabled {
        bail!("task {task_id} is disabled");
    }

    let behavior_query = format!(
        r#"query {{
            AgentBehavior(filter: {{
                agent_did: {{ _eq: "{agent_did}" }},
                behavior_id: {{ _eq: "{id}" }}
            }}, limit: 1) {{
                agent_did
                enabled
            }}
        }}"#,
        id = escape_graphql_string(behavior_id),
        agent_did = escape_graphql_string(agent_did),
    );
    let behavior_response = node.execute(&behavior_query).await;
    if behavior_response.has_errors() {
        bail!(
            "fetch behavior for task {task_id} failed: {:?}",
            behavior_response.errors
        );
    }
    let behavior_row = behavior_response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentBehavior"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| {
            anyhow!(
                "no AgentBehavior with behavior_id = {} (referenced by task {task_id})",
                behavior_id
            )
        })?;
    let behavior_agent_did = behavior_row
        .get("agent_did")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("AgentBehavior {behavior_id} has no agent_did"))?;
    if behavior_agent_did != agent_did {
        bail!("AgentBehavior {behavior_id} belongs to a different principal");
    }
    if !behavior_row
        .get("enabled")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        bail!("AgentBehavior {behavior_id} is disabled");
    }

    enqueue_task_now(node, actor, task_row, args).await
}

async fn execute_config_rows<T: DeserializeOwned>(
    access: &ConfigAccess,
    root: &str,
    query: &str,
    operation: &str,
) -> Result<Vec<T>> {
    let body = access
        .execute(query)
        .await
        .with_context(|| operation.to_string())?;
    if body
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        bail!(
            "{operation} returned errors: {}",
            body.get("errors").unwrap_or(&Value::Null)
        );
    }
    let rows = body
        .get("data")
        .and_then(|data| data.get(root))
        .and_then(Value::as_array)
        .with_context(|| format!("{operation} returned no {root} rows"))?;
    rows.iter()
        .cloned()
        .map(|row| {
            serde_json::from_value(row)
                .with_context(|| format!("deserialize {root} for {operation}"))
        })
        .collect()
}

async fn load_task_on(access: &ConfigAccess, agent_did: &str, task_id: &str) -> Result<Task> {
    let query = format!(
        r#"query {{
            Task(filter: {{
                agent_did: {{ _eq: "{agent_did}" }},
                task_id: {{ _eq: "{task_id}" }}
            }}, limit: 2) {{
                task_id
                agent_did
                display_name
                description
                behavior_id
                prompt_template
                goal_objective_template
                goal_token_budget
                hooks
                enabled
                output_schema_ref
                created_at
                updated_at
                tags
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
        task_id = escape_graphql_string(task_id),
    );
    let mut rows = execute_config_rows(access, "Task", &query, "load canonical manual task")
        .await?
        .into_iter();
    let row = rows
        .next()
        .ok_or_else(|| anyhow!("task {task_id} was not found for {agent_did}"))?;
    if rows.next().is_some() {
        bail!("task {task_id} is ambiguous for {agent_did}");
    }
    Ok(row)
}

async fn ensure_behavior_enabled_on(
    access: &ConfigAccess,
    agent_did: &str,
    behavior_id: &str,
) -> Result<()> {
    let query = format!(
        r#"query {{
            AgentBehavior(filter: {{
                agent_did: {{ _eq: "{agent_did}" }},
                behavior_id: {{ _eq: "{behavior_id}" }}
            }}, limit: 2) {{
                agent_did
                behavior_id
                enabled
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
    );
    #[derive(serde::Deserialize)]
    struct BehaviorState {
        agent_did: String,
        behavior_id: String,
        enabled: bool,
    }
    let mut rows = execute_config_rows::<BehaviorState>(
        access,
        "AgentBehavior",
        &query,
        "load canonical task behavior",
    )
    .await?
    .into_iter();
    let row = rows.next().ok_or_else(|| {
        anyhow!("no AgentBehavior with behavior_id = {behavior_id} for {agent_did}")
    })?;
    if rows.next().is_some() {
        bail!("AgentBehavior {behavior_id} is ambiguous for {agent_did}");
    }
    if row.agent_did != agent_did || row.behavior_id != behavior_id {
        bail!("AgentBehavior {behavior_id} belongs to a different principal");
    }
    if !row.enabled {
        bail!("AgentBehavior {behavior_id} is disabled");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManualTaskInvocation {
    pub agent_did: String,
    pub task_id: String,
    pub behavior_id: String,
    pub content: String,
    pub goal_objective: Option<String>,
    pub goal_token_budget: Option<i64>,
    pub session_title: String,
}

fn render_task_invocation(task: &Task, args: serde_json::Value) -> Result<ManualTaskInvocation> {
    let task_id = normalize_required("task_id", &task.task_id)?;
    let agent_did = normalize_required("agent_did", &task.agent_did)?;
    let behavior_id = normalize_required("behavior_id", &task.behavior_id)?;
    let prompt_template = normalize_required("prompt_template", &task.prompt_template)?;
    if !task.enabled {
        bail!("task {task_id} is disabled");
    }
    gents::goal::validate_task_goal_declaration(
        task.goal_objective_template.as_deref(),
        task.goal_token_budget,
    )?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let (node_scope, ctx_scope) = gents::template::task_node_ctx(agent_did, behavior_id, &now);
    let scope = gents::TemplateScope {
        event: serde_json::json!({
            "fired_at": now,
            "trigger_id": serde_json::Value::Null,
            "trigger_kind": "manual",
        }),
        doc: None,
        args: Some(args),
        group: None,
        node: node_scope,
        ctx: ctx_scope,
    };
    let content = gents::render_template(prompt_template, &scope)
        .map_err(|error| anyhow!("render manual template for task {task_id}: {error}"))?;
    let goal_objective = task
        .goal_objective_template
        .as_deref()
        .map(str::trim)
        .filter(|template| !template.is_empty())
        .map(|template| {
            gents::render_template(template, &scope)
                .map_err(|error| anyhow!("render goal template for task {task_id}: {error}"))
        })
        .transpose()?;
    if goal_objective
        .as_deref()
        .is_some_and(|objective| objective.trim().is_empty())
    {
        bail!("task {task_id} rendered an empty goal objective");
    }
    let task_label = task
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(task_id);
    Ok(ManualTaskInvocation {
        agent_did: agent_did.to_string(),
        task_id: task_id.to_string(),
        behavior_id: behavior_id.to_string(),
        content,
        goal_objective,
        goal_token_budget: task.goal_token_budget,
        session_title: task_session_title(task_label),
    })
}

/// Resolve operator-owned task configuration from its canonical control plane.
/// The caller submits the returned invocation through the ordinary client
/// request authority so requester-scoped P2P filters admit it.
pub async fn resolve_task_now_on(
    config_access: &ConfigAccess,
    agent_did: &str,
    task_id: &str,
    args: serde_json::Value,
) -> Result<ManualTaskInvocation> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let task_id = normalize_required("task_id", task_id)?;
    let task = load_task_on(config_access, agent_did, task_id).await?;
    if !task.enabled {
        bail!("task {task_id} is disabled");
    }
    let behavior_id = normalize_required("behavior_id", &task.behavior_id)?;
    ensure_behavior_enabled_on(config_access, agent_did, behavior_id).await?;
    render_task_invocation(&task, args)
}

async fn enqueue_task_now(
    node: &EmbeddedNode,
    actor: identity::Did,
    task_row: &Task,
    args: serde_json::Value,
) -> Result<String> {
    let task_id = normalize_required("task_id", &task_row.task_id)?;
    let agent_did = normalize_required("agent_did", &task_row.agent_did)?;
    let behavior_id = normalize_required("behavior_id", &task_row.behavior_id)?;
    let prompt_template = normalize_required("prompt_template", &task_row.prompt_template)?;
    if !task_row.enabled {
        bail!("task {task_id} is disabled");
    }

    let task_label = task_row
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(task_id);
    let conversation_title = task_session_title(task_label);

    let goal_objective_template = task_row.goal_objective_template.as_deref();
    gents::goal::validate_task_goal_declaration(
        goal_objective_template,
        task_row.goal_token_budget,
    )?;
    if let Some(goal_objective_template) = goal_objective_template {
        let goal_objective_template = goal_objective_template.trim();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let (node_scope, ctx_scope) = gents::template::task_node_ctx(agent_did, behavior_id, &now);
        let scope = gents::TemplateScope {
            event: serde_json::json!({
                "fired_at": now,
                "trigger_id": serde_json::Value::Null,
                "trigger_kind": "manual",
            }),
            doc: None,
            args: Some(args),
            group: None,
            node: node_scope,
            ctx: ctx_scope,
        };
        let content = gents::render_template(prompt_template, &scope)
            .map_err(|error| anyhow!("render manual template for task {task_id}: {error}"))?;
        let objective = gents::render_template(goal_objective_template, &scope)
            .map_err(|error| anyhow!("render goal template for task {task_id}: {error}"))?;
        if objective.trim().is_empty() {
            bail!("task {task_id} rendered an empty goal objective");
        }
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let identity = gents::goal::task_goal_fire_identity(
            agent_did,
            task_id,
            &format!("desktop-manual:{invocation_id}"),
        );
        let create = gents::build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
            agent_did,
            behavior_id,
            &content,
            gents::lifecycle::ExecutionOrigin::Interactive,
            gents::lifecycle::TriggerLineage {
                trigger_id: None,
                trigger_kind: Some("manual".to_string()),
                source_doc_id: None,
                correlation: None,
                trigger_context: None,
            },
            Some(&conversation_title),
            None,
            &identity.request_id,
            &identity.session_id,
            Some(&identity.retry_key),
            None,
            None,
        )
        .await?;
        let enqueued = gents::goal::submit_goal_backed_request_local(
            node,
            actor,
            agent_did,
            &identity.session_id,
            &objective,
            task_row.goal_token_budget,
            &create,
        )
        .await?;
        return Ok(enqueued.doc_id);
    }

    write_manual_agent_request_with_conversation_title(
        node,
        actor,
        agent_did,
        behavior_id,
        task_id,
        prompt_template,
        args,
        Some(&conversation_title),
    )
    .await
}

/// Fire a schedule's task immediately.
///
/// Operator override = manual run of the schedule's task with empty
/// args. The resulting `AgentRequest` carries `caused_by_trigger_kind =
/// "manual"`, NOT `"schedule"` — this is an explicit operator override,
/// not a cron fire, so observers can cleanly separate "the scheduler
/// decided to fire" from "a human pressed Run Now on the Schedule row."
///
/// We load the canonical `Task` from GraphQL directly rather than from the
/// desktop store, so this path stays correct even if the store is
/// stale (e.g., the schedule was just created and the watcher has not
/// caught up yet). The `SELECT` mirrors every field on
/// canonical document so `serde_json::from_value` sees the authoritative shape.
pub async fn fire_schedule_now(
    node: &EmbeddedNode,
    actor: identity::Did,
    schedule: &Schedule,
) -> Result<String> {
    let agent_did = normalize_required("agent_did", &schedule.agent_did)?;
    let schedule_id = normalize_required("schedule_id", &schedule.schedule_id)?;
    let trigger_query = format!(
        r#"query {{
            Trigger(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{
                agent_did
                trigger_id
                task_id
                display_name
                description
                source
                enabled
                concurrency
                created_at
                updated_at
                tags
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let trigger_response = node.execute(&trigger_query).await;
    if trigger_response.has_errors() {
        bail!(
            "fetch triggers for schedule {schedule_id} failed: {:?}",
            trigger_response.errors
        );
    }
    let mut matching = trigger_response
        .data
        .as_ref()
        .and_then(|data| data.get("Trigger"))
        .and_then(|rows| rows.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<Trigger>(value.clone()).ok())
        .filter(|trigger| {
            trigger.enabled
                && matches!(
                    &trigger.source,
                    gents::document_config::TriggerSource::Schedule { schedule_id: id }
                        if id == schedule_id
                )
        });
    let trigger = matching
        .next()
        .ok_or_else(|| anyhow!("schedule {schedule_id} has no enabled Trigger"))?;
    if matching.next().is_some() {
        bail!("schedule {schedule_id} has multiple enabled Triggers; run a Trigger explicitly");
    }
    let task_id = trigger.task_id.as_str();
    let task_query = format!(
        r#"query {{
            Task(filter: {{ task_id: {{ _eq: "{id}" }} }}, limit: 1) {{
                task_id
                agent_did
                display_name
                description
                behavior_id
                prompt_template
                goal_objective_template
                goal_token_budget
                hooks
                enabled
                output_schema_ref
                created_at
                updated_at
                tags
            }}
        }}"#,
        id = escape_graphql_string(task_id),
    );
    let task_response = node.execute(&task_query).await;
    if task_response.has_errors() {
        bail!(
            "fetch task for schedule {schedule_id} failed: {:?}",
            task_response.errors,
        );
    }
    let task_row_json = task_response
        .data
        .as_ref()
        .and_then(|d| d.get("Task"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow!("task {task_id} not found"))?;
    let task_row: Task = serde_json::from_value(task_row_json.clone())
        .map_err(|e| anyhow!("deserialize Task: {e}"))?;

    fire_task_now(node, actor, &task_row, serde_json::json!({})).await
}

/// Resolve the schedule, enabled trigger, task, and behavior from canonical
/// operator access. Submission remains owned by the client request path.
pub async fn resolve_schedule_now_on(
    config_access: &ConfigAccess,
    agent_did: &str,
    schedule_id: &str,
) -> Result<ManualTaskInvocation> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let schedule_id = normalize_required("schedule_id", schedule_id)?;
    let schedule_query = format!(
        r#"query {{
            Schedule(filter: {{
                agent_did: {{ _eq: "{agent_did}" }},
                schedule_id: {{ _eq: "{schedule_id}" }}
            }}, limit: 2) {{
                agent_did
                schedule_id
                cadence
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
        schedule_id = escape_graphql_string(schedule_id),
    );
    let schedules = execute_config_rows::<Schedule>(
        config_access,
        "Schedule",
        &schedule_query,
        "load canonical manual schedule",
    )
    .await?;
    if schedules.len() != 1 {
        bail!(
            "schedule {schedule_id} was {} for {agent_did}",
            if schedules.is_empty() {
                "not found"
            } else {
                "ambiguous"
            }
        );
    }

    let trigger_query = format!(
        r#"query {{
            Trigger(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{
                agent_did
                trigger_id
                task_id
                display_name
                description
                source
                enabled
                concurrency
                created_at
                updated_at
                tags
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let triggers = execute_config_rows::<Trigger>(
        config_access,
        "Trigger",
        &trigger_query,
        "load canonical schedule triggers",
    )
    .await?;
    let mut matching = triggers.into_iter().filter(|trigger| {
        trigger.enabled
            && matches!(
                &trigger.source,
                gents::document_config::TriggerSource::Schedule { schedule_id: id }
                    if id == schedule_id
            )
    });
    let trigger = matching
        .next()
        .ok_or_else(|| anyhow!("schedule {schedule_id} has no enabled Trigger"))?;
    if matching.next().is_some() {
        bail!("schedule {schedule_id} has multiple enabled Triggers; run a Trigger explicitly");
    }
    let task = load_task_on(config_access, agent_did, &trigger.task_id).await?;
    if !task.enabled {
        bail!("task {} is disabled", task.task_id);
    }
    let behavior_id = normalize_required("behavior_id", &task.behavior_id)?;
    ensure_behavior_enabled_on(config_access, agent_did, behavior_id).await?;
    render_task_invocation(&task, serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn canonical_task_resolution_ignores_stale_desktop_revision() -> Result<()> {
        let operator = Arc::new(EmbeddedNode::builder().build().await?);
        let desktop = EmbeddedNode::builder().build().await?;
        gents::ensure_runtime_schemas(operator.as_ref()).await?;
        gents::ensure_runtime_schemas(&desktop).await?;
        let owner = "did:test:canonical-task-run";
        let config: gents::document_config::PackConfig = serde_json::from_value(json!({
            "agent_principal":{"agent_did":owner},
            "inference_backends":[{"agent_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agent_behaviors":[{"agent_did":owner,"behavior_id":"behavior","inference_profile_id":"profile"}]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
        for node in [operator.as_ref(), &desktop] {
            ConfigAccess::transact_local(node, None, "desktop.canonical_task.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        }

        let canonical: Task = serde_json::from_value(json!({
            "agent_did":owner,
            "task_id":"task",
            "behavior_id":"behavior",
            "prompt_template":"Canonical {{ args.item }}",
            "enabled":true
        }))?;
        upsert_task(operator.as_ref(), &canonical).await?;
        let mut stale = canonical.clone();
        stale.enabled = false;
        stale.prompt_template = "Stale {{ args.item }}".into();
        upsert_task(&desktop, &stale).await?;

        let operator_access = ConfigAccess::Local(Arc::clone(&operator));
        let invocation =
            resolve_task_now_on(&operator_access, owner, "task", json!({"item":"truth"})).await?;
        assert_eq!(invocation.content, "Canonical truth");
        assert_eq!(invocation.agent_did, owner);
        assert_eq!(invocation.behavior_id, "behavior");

        let operator_response = operator
            .execute("query { AgentRequest { request_id } }")
            .await;
        assert!(
            !operator_response.has_errors(),
            "{:?}",
            operator_response.errors
        );
        assert_eq!(
            operator_response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        let desktop_response = desktop
            .execute("query { AgentRequest { request_id } }")
            .await;
        assert!(
            !desktop_response.has_errors(),
            "{:?}",
            desktop_response.errors
        );
        assert_eq!(
            desktop_response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn automation_replacements_preserve_scoped_references_and_goal_hook_rules() -> Result<()>
    {
        let node = EmbeddedNode::builder().build().await?;
        gents::ensure_runtime_schemas(&node).await?;
        for owner in ["did:test:automation-a", "did:test:automation-b"] {
            let config: gents::document_config::PackConfig = serde_json::from_value(json!({
                "agent_principal":{"agent_did":owner},
                "inference_backends":[{"agent_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
                "inference_profiles":[{"agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
                "agent_behaviors":[{"agent_did":owner,"behavior_id":"behavior","inference_profile_id":"profile"}]
            }))?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            ConfigAccess::transact_local(&node, None, "desktop.automation.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        }
        let owner = "did:test:automation-a";
        let mut task: Task = serde_json::from_value(
            json!({"agent_did":owner,"task_id":"task","behavior_id":"behavior","prompt_template":"Do work","goal_objective_template":"Finish work","goal_token_budget":10000,"hooks":[{"hook_id":"prepare","phase":"before","command":["true"]}]}),
        )?;
        upsert_task(&node, &task).await?;
        task.goal_objective_template = None;
        assert!(upsert_task(&node, &task).await.is_err());
        task.goal_token_budget = None;
        upsert_task(&node, &task).await?;
        task.hooks[0].timeout_secs = Some(0);
        assert!(upsert_task(&node, &task).await.is_err());
        task.hooks[0].timeout_secs = None;
        let source: EventSource = serde_json::from_value(
            json!({"agent_did":owner,"event_source_id":"source","source_collection":"AgentRequest"}),
        )?;
        upsert_event_source(&node, &source).await?;
        let mut trigger: Trigger = serde_json::from_value(
            json!({"agent_did":owner,"trigger_id":"trigger","task_id":"task","source":{"kind":"event","event_source_id":"source"}}),
        )?;
        upsert_trigger(&node, &trigger).await?;
        assert!(delete_task(&node, owner, "task").await.is_err());
        assert!(delete_event_source(&node, owner, "source").await.is_err());
        // A foreign principal with the same behavior label cannot borrow this task/source.
        trigger.agent_did = "did:test:automation-b".into();
        assert!(upsert_trigger(&node, &trigger).await.is_err());
        assert_eq!(
            delete_trigger(&node, "did:test:automation-b", "trigger").await?,
            0
        );
        trigger.agent_did = owner.into();
        let schedule: Schedule = serde_json::from_value(
            json!({"agent_did":owner,"schedule_id":"schedule","cadence":{"kind":"interval","interval_secs":60}}),
        )?;
        upsert_schedule(&node, &schedule).await?;
        trigger.source = gents::document_config::TriggerSource::Schedule {
            schedule_id: "schedule".into(),
        };
        upsert_trigger(&node, &trigger).await?;
        assert_eq!(delete_event_source(&node, owner, "source").await?, 1);
        assert!(delete_schedule(&node, owner, "schedule").await.is_err());
        ConfigAccess::transact_local(&node, None, "desktop.automation.verify", |txn| {
            Box::pin(async move {
                let (_, value) =
                    read_desired_state_record_in_txn(txn, Collection::Task, owner, "task")
                        .await?
                        .unwrap();
                let saved: Task = serde_json::from_value(value)?;
                assert!(saved.goal_objective_template.is_none());
                assert!(saved.goal_token_budget.is_none());
                assert_eq!(saved.hooks.len(), 1);
                assert_eq!(saved.hooks[0].command, vec!["true"]);
                assert!(saved.hooks[0].timeout_secs.is_none());
                Ok(())
            })
        })
        .await?;
        assert_eq!(delete_trigger(&node, owner, "trigger").await?, 1);
        assert_eq!(delete_task(&node, owner, "task").await?, 1);
        assert_eq!(delete_schedule(&node, owner, "schedule").await?, 1);
        Ok(())
    }
}
