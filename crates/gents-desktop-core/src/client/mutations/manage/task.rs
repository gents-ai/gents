//! Canonical automation configuration through the shared candidate transaction.
//! Runtime task invocation below remains a separate integration surface.
use super::super::graphql::{escape_graphql_string, normalize_required};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use gents::collection::Collection;
use gents::config_client::{
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan, apply_desired_state_plan,
    read_desired_state_record_in_txn,
};
use gents::document_config::{EventSource, Schedule, Task, Trigger};
use gents::{task_session_title, write_manual_agent_request_with_conversation_title};
use gents_protocol::row::{ScheduleRow, TaskRow};

pub async fn upsert_task(node: &EmbeddedNode, document: &Task) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Task,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.task.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_task(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::Task,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.task.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed = read_desired_state_record_in_txn(txn, Collection::Task, agent_did, id)
                .await?
                .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

pub async fn upsert_schedule(node: &EmbeddedNode, document: &Schedule) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Schedule,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.schedule.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_schedule(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::Schedule,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.schedule.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed =
                read_desired_state_record_in_txn(txn, Collection::Schedule, agent_did, id)
                    .await?
                    .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

pub async fn upsert_trigger(node: &EmbeddedNode, document: &Trigger) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Trigger,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.trigger.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_trigger(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::Trigger,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.trigger.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed = read_desired_state_record_in_txn(txn, Collection::Trigger, agent_did, id)
                .await?
                .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

pub async fn upsert_event_source(node: &EmbeddedNode, document: &EventSource) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::EventSource,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.event_source.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_event_source(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::EventSource,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.event_source.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed =
                read_desired_state_record_in_txn(txn, Collection::EventSource, agent_did, id)
                    .await?
                    .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
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
    task_row: &TaskRow,
    args: serde_json::Value,
) -> Result<String> {
    let task_id = normalize_required("task_id", &task_row.task_id)?;
    let behavior_id = task_row
        .behavior_id
        .as_deref()
        .and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .ok_or_else(|| anyhow!("task {task_id} has no behavior_id"))?;
    let prompt_template = task_row
        .prompt_template
        .as_deref()
        .ok_or_else(|| anyhow!("task {task_id} has no prompt_template"))?;
    if !task_row.enabled.unwrap_or(false) {
        bail!("task {task_id} is disabled");
    }

    let behavior_query = format!(
        r#"query {{
            AgentBehavior(filter: {{ behavior_id: {{ _eq: "{id}" }} }}, limit: 1) {{
                agent_did
                enabled
            }}
        }}"#,
        id = escape_graphql_string(behavior_id),
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
    let agent_did = behavior_row
        .get("agent_did")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("AgentBehavior {behavior_id} has no agent_did"))?;
    if !behavior_row
        .get("enabled")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        bail!("AgentBehavior {behavior_id} is disabled");
    }

    let task_label = task_row
        .name
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
/// We load the `TaskRow` from GraphQL directly rather than from the
/// desktop store, so this path stays correct even if the store is
/// stale (e.g., the schedule was just created and the watcher has not
/// caught up yet). The `SELECT` mirrors every field on
/// `gents_protocol::row::TaskRow` so `serde_json::from_value`
/// does not fail on a missing column.
pub async fn fire_schedule_now(node: &EmbeddedNode, schedule_row: &ScheduleRow) -> Result<String> {
    let task_id = schedule_row
        .task_id
        .as_deref()
        .and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .ok_or_else(|| anyhow!("schedule {} has no task_id", schedule_row.schedule_id))?;
    let task_query = format!(
        r#"query {{
            Task(filter: {{ task_id: {{ _eq: "{id}" }} }}, limit: 1) {{
                task_id
                name
                description
                behavior_id
                prompt_template
                goal_objective_template
                goal_token_budget
                enabled
                output_schema_ref
                created_at
                updated_at
            }}
        }}"#,
        id = escape_graphql_string(task_id),
    );
    let task_response = node.execute(&task_query).await;
    if task_response.has_errors() {
        bail!(
            "fetch task for schedule {schedule_id} failed: {:?}",
            task_response.errors,
            schedule_id = schedule_row.schedule_id,
        );
    }
    let task_row_json = task_response
        .data
        .as_ref()
        .and_then(|d| d.get("Task"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow!("task {task_id} not found"))?;
    let task_row: TaskRow = serde_json::from_value(task_row_json.clone())
        .map_err(|e| anyhow!("deserialize TaskRow: {e}"))?;

    fire_task_now(node, &task_row, serde_json::json!({})).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
