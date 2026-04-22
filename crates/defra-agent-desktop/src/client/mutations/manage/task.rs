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
use chrono::Utc;
use defra_agent::write_manual_agent_request;
use defra_agent_protocol::row::{EventTriggerRow, ScheduleRow, TaskRow};
use defra_node::EmbeddedNode;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field,
    graphql_optional_int_field, graphql_string_field, join_fields, normalize_required,
};

pub async fn upsert_task(node: &EmbeddedNode, row: &TaskRow) -> Result<()> {
    let task_id = normalize_required("task_id", &row.task_id)?;
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
    // CRITICAL: only apply-owned fields may appear in the mutation input.
    // Runtime-owned fields (`next_run_at`, `last_attempt_at`,
    // `last_status`, `last_error`, `fire_count`) belong to the scheduler
    // and must never be set from the desktop apply path — otherwise
    // reapplying a Schedule edit would wipe the engine's bookkeeping.
    let schedule_id = normalize_required("schedule_id", &row.schedule_id)?;
    let task_id = normalize_required(
        "task_id",
        row.task_id
            .as_deref()
            .context("task_id is required for Schedule")?,
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
            r#"schedule_id: "{}""#,
            escape_graphql_string(schedule_id)
        )),
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(graphql_optional_int_field(
            "interval_secs",
            row.interval_secs,
        )),
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
        Some(graphql_optional_int_field(
            "interval_secs",
            row.interval_secs,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_field(
            "concurrency",
            row.concurrency.as_deref(),
        )),
        Some(format!(r#"updated_at: "{}""#, escape_graphql_string(&now))),
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

    // Resolve `agent_did` via GraphQL. The desktop `ClientStore` has
    // behavior rows cached, but a fresh lookup keeps this writer correct
    // even if the store is stale, and mirrors the CLI path's shape.
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

    write_manual_agent_request(
        node,
        agent_did,
        behavior_id,
        task_id,
        prompt_template,
        args,
    )
    .await
}

/// Manual "Run Now" path for a Schedule.
///
/// The scheduler's manual-run surface lands in PR 3; for PR 1 we
/// surface the gap explicitly so the desktop button reports a
/// meaningful error instead of silently reaching into runtime-owned
/// fields.
pub async fn fire_schedule_now(_node: &EmbeddedNode, _row: &ScheduleRow) -> Result<()> {
    bail!("manual Schedule fire lands in PR 3; this path writes no runtime-owned fields")
}

/// Apply-path upsert for an `EventTrigger` document.
///
/// CRITICAL: only apply-owned fields may appear in the mutation input.
/// Runtime-owned fields (`last_attempt_at`, `last_fired_source_doc_id`,
/// `last_status`, `last_error`, `fire_count`) are written exclusively by
/// the trigger engine. Projecting them here would let a desktop edit
/// clobber the engine's bookkeeping on every re-apply. The CLI's
/// `config_writes/event_trigger.rs` enforces the same contract for
/// manifest-apply; this desktop path mirrors it for in-app edits.
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
        Some(format!(
            r#"task_id: "{}""#,
            escape_graphql_string(task_id)
        )),
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
        Some(format!(
            r#"task_id: "{}""#,
            escape_graphql_string(task_id)
        )),
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
            r#"updated_at: "{}""#,
            escape_graphql_string(&now)
        )),
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
