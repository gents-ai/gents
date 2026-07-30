//! Task and Schedule mutations for the desktop client.
//!
//! Task 52 wires real upsert mutations for the `Task` and `Schedule`
//! collections. The schemas are apply-owned for `Task` (every field),
//! and apply-owned-plus-runtime-owned for `Schedule`. This writer must
//! only ever project apply-owned fields into the mutation input —
//! runtime-owned fields (`next_run_at`, `last_attempt_at`,
//! `last_status`, `last_error`, `fire_count`) are the scheduler's
//! responsibility, and re-applying a desktop edit must never clobber
//! them.
//!
//! `Schedule.created_at` / `updated_at` are intentionally omitted from
//! the desktop write path for now. DefraDB currently round-trips those
//! DateTime fields as plain strings when written through this upsert
//! shape, and the trigger engine's later `update_Schedule` bookkeeping
//! mutations then fail schema validation on the existing document. The
//! runtime does not require these timestamps, so leaving them unset is
//! safer than creating schedules the engine cannot advance.
//!
//! The `fire_schedule_now` path is deliberately left as an error until
//! the manual-run surface lands in PR 3. The desktop can still show the
//! "Run Now" button, but invoking it surfaces the intentional gap
//! rather than silently mutating runtime state.
//!
//! These mutations target the embedded DefraDB node via
//! `upsert_Task` / `upsert_Schedule`, mirroring the simpler shape
//! `behavior.rs` and the other manage writers already use. The CLI's
//! `config_writes/task.rs` and `config_writes/schedule.rs` carry a more
//! defensive create/update split for manifest-apply flows; the desktop
//! only needs the upsert path today.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use gents::{task_run_conversation_title, write_manual_agent_request_with_conversation_title};
use gents_protocol::graphql::normalize_optional_rfc3339;
use gents_protocol::row::{EventTriggerRow, ScheduleRow, TaskRow};
use serde_json::Value;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, execute_remote_delete_mutation,
    graphql_optional_bool_field, graphql_optional_int_field, graphql_string_field, join_fields,
    normalize_required,
};

pub async fn upsert_task(node: &EmbeddedNode, row: &TaskRow) -> Result<()> {
    let task_id = normalize_required("task_id", &row.task_id)?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let created_at = row.created_at.as_deref();
    let created_at = normalize_optional_rfc3339(created_at)?.unwrap_or_else(|| now.clone());
    let updated_at = row.updated_at.as_deref();
    let updated_at = normalize_optional_rfc3339(updated_at)?.unwrap_or_else(|| now.clone());

    let add_fields = [
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "description",
            row.description.as_deref(),
        )),
        Some(graphql_string_field(
            "behavior_id",
            row.behavior_id.as_deref(),
        )),
        Some(graphql_string_field(
            "prompt_template",
            row.prompt_template.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_field(
            "output_schema_ref",
            row.output_schema_ref.as_deref(),
        )),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
        Some(format!(
            r#"updated_at: "{}""#,
            escape_graphql_string(&updated_at)
        )),
    ];
    let update_fields = [
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "description",
            row.description.as_deref(),
        )),
        Some(graphql_string_field(
            "behavior_id",
            row.behavior_id.as_deref(),
        )),
        Some(graphql_string_field(
            "prompt_template",
            row.prompt_template.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_field(
            "output_schema_ref",
            row.output_schema_ref.as_deref(),
        )),
        Some(format!(r#"updated_at: "{}""#, escape_graphql_string(&now))),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_Task(
                filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        task_id = escape_graphql_string(task_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_task").await
}

pub async fn upsert_schedule(node: &EmbeddedNode, row: &ScheduleRow) -> Result<()> {
    let schedule_id = normalize_required("schedule_id", &row.schedule_id)?;
    let task_id = normalize_required(
        "task_id",
        row.task_id
            .as_deref()
            .context("task_id is required for Schedule")?,
    )?;
    let add_fields = [
        Some(format!(
            r#"schedule_id: "{}""#,
            escape_graphql_string(schedule_id)
        )),
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(graphql_optional_int_field(
            "interval_secs",
            row.interval_secs,
        )),
        Some(graphql_string_field("cron", row.cron.as_deref())),
        Some(graphql_string_field("timezone", row.timezone.as_deref())),
        Some(graphql_string_field(
            "missed_run_policy",
            row.missed_run_policy.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_field(
            "concurrency",
            row.concurrency.as_deref(),
        )),
    ];
    let update_fields = [
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(graphql_optional_int_field(
            "interval_secs",
            row.interval_secs,
        )),
        Some(graphql_string_field("cron", row.cron.as_deref())),
        Some(graphql_string_field("timezone", row.timezone.as_deref())),
        Some(graphql_string_field(
            "missed_run_policy",
            row.missed_run_policy.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_field(
            "concurrency",
            row.concurrency.as_deref(),
        )),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_Schedule(
                filter: {{ schedule_id: {{ _eq: "{schedule_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        schedule_id = escape_graphql_string(schedule_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_schedule").await
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
    let conversation_title = task_run_conversation_title(task_label);

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

pub async fn upsert_event_trigger(node: &EmbeddedNode, row: &EventTriggerRow) -> Result<()> {
    let trigger_id = normalize_required("trigger_id", &row.trigger_id)?;
    let task_id = normalize_required(
        "task_id",
        row.task_id
            .as_deref()
            .context("task_id is required for EventTrigger")?,
    )?;
    let now = Utc::now().to_rfc3339();
    let created_at = row
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| now.clone());
    let updated_at = row
        .updated_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| now.clone());

    let add_fields = [
        Some(format!(
            r#"trigger_id: "{}""#,
            escape_graphql_string(trigger_id)
        )),
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(graphql_string_field(
            "source_collection",
            row.source_collection.as_deref(),
        )),
        Some(graphql_string_field(
            "event_kind",
            row.event_kind.as_deref(),
        )),
        Some(graphql_string_field("filter", row.filter.as_deref())),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_field(
            "concurrency",
            row.concurrency.as_deref(),
        )),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
        Some(format!(
            r#"updated_at: "{}""#,
            escape_graphql_string(&updated_at)
        )),
    ];
    let update_fields = [
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(graphql_string_field(
            "source_collection",
            row.source_collection.as_deref(),
        )),
        Some(graphql_string_field(
            "event_kind",
            row.event_kind.as_deref(),
        )),
        Some(graphql_string_field("filter", row.filter.as_deref())),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_field(
            "concurrency",
            row.concurrency.as_deref(),
        )),
        Some(format!(r#"updated_at: "{}""#, escape_graphql_string(&now))),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{trigger_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        trigger_id = escape_graphql_string(trigger_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_event_trigger").await
}

pub async fn delete_task(node: &EmbeddedNode, task_id: &str) -> Result<usize> {
    let mutation = build_delete_task_mutation(task_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_task failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_Task"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

pub async fn delete_task_from_graphql(graphql: &str, task_id: &str) -> Result<usize> {
    let graphql = normalize_required("graphql", graphql)?;
    let mutation = build_delete_task_mutation(task_id)?;
    execute_remote_delete_mutation(graphql, &mutation, "delete_task", "delete_Task").await
}

fn build_delete_task_mutation(task_id: &str) -> Result<String> {
    let task_id = normalize_required("task_id", task_id)?;
    let task_id = escape_graphql_string(task_id);
    Ok(format!(
        r#"mutation {{
            delete_Task(
                filter: {{ task_id: {{ _eq: "{task_id}" }} }}
            ) {{ _docID }}
        }}"#
    ))
}

pub async fn delete_schedule(node: &EmbeddedNode, schedule_id: &str) -> Result<usize> {
    let mutation = build_delete_schedule_mutation(schedule_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_schedule failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_Schedule"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

pub async fn delete_schedule_from_graphql(graphql: &str, schedule_id: &str) -> Result<usize> {
    let graphql = normalize_required("graphql", graphql)?;
    let mutation = build_delete_schedule_mutation(schedule_id)?;
    execute_remote_delete_mutation(graphql, &mutation, "delete_schedule", "delete_Schedule").await
}

fn build_delete_schedule_mutation(schedule_id: &str) -> Result<String> {
    let schedule_id = normalize_required("schedule_id", schedule_id)?;
    let schedule_id = escape_graphql_string(schedule_id);
    Ok(format!(
        r#"mutation {{
            delete_Schedule(
                filter: {{ schedule_id: {{ _eq: "{schedule_id}" }} }}
            ) {{ _docID }}
        }}"#
    ))
}

pub async fn delete_event_trigger(node: &EmbeddedNode, trigger_id: &str) -> Result<usize> {
    let mutation = build_delete_event_trigger_mutation(trigger_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_event_trigger failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_EventTrigger"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

pub async fn delete_event_trigger_from_graphql(graphql: &str, trigger_id: &str) -> Result<usize> {
    let graphql = normalize_required("graphql", graphql)?;
    let mutation = build_delete_event_trigger_mutation(trigger_id)?;
    execute_remote_delete_mutation(
        graphql,
        &mutation,
        "delete_event_trigger",
        "delete_EventTrigger",
    )
    .await
}

fn build_delete_event_trigger_mutation(trigger_id: &str) -> Result<String> {
    let trigger_id = normalize_required("trigger_id", trigger_id)?;
    let trigger_id = escape_graphql_string(trigger_id);
    Ok(format!(
        r#"mutation {{
            delete_EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{trigger_id}" }} }}
            ) {{ _docID }}
        }}"#
    ))
}
