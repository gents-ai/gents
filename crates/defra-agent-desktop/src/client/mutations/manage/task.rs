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

use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_agent_protocol::row::{ScheduleRow, TaskRow};
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
        Some(format!(
            r#"task_id: "{}""#,
            escape_graphql_string(task_id)
        )),
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
        Some(format!(
            r#"updated_at: "{}""#,
            escape_graphql_string(&now)
        )),
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
        Some(format!(
            r#"task_id: "{}""#,
            escape_graphql_string(task_id)
        )),
        Some(graphql_optional_int_field("interval_secs", row.interval_secs)),
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
        Some(graphql_optional_int_field("interval_secs", row.interval_secs)),
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

/// Manual "Run Now" path for a Schedule.
///
/// The scheduler's manual-run surface lands in PR 3; for PR 1 we
/// surface the gap explicitly so the desktop button reports a
/// meaningful error instead of silently reaching into runtime-owned
/// fields.
pub async fn fire_schedule_now(_node: &EmbeddedNode, _row: &ScheduleRow) -> Result<()> {
    bail!("manual Schedule fire lands in PR 3; this path writes no runtime-owned fields")
}
