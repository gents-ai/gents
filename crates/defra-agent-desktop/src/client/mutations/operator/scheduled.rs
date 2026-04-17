use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_agent_protocol::row::ScheduledTaskRow;
use defra_node::EmbeddedNode;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field,
    graphql_optional_int_field, graphql_string_field, join_fields, normalize_required,
};

pub async fn upsert_scheduled_task(node: &EmbeddedNode, row: &ScheduledTaskRow) -> Result<()> {
    let task_id = normalize_required("task_id", &row.task_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for ScheduledTask")?,
    )?;
    let behavior_id = normalize_required(
        "behavior_id",
        row.behavior_id
            .as_deref()
            .context("behavior_id is required for ScheduledTask")?,
    )?;
    let name = normalize_required(
        "name",
        row.name
            .as_deref()
            .context("name is required for ScheduledTask")?,
    )?;
    let prompt = normalize_required(
        "prompt",
        row.prompt
            .as_deref()
            .context("prompt is required for ScheduledTask")?,
    )?;
    let interval_secs = row
        .interval_secs
        .context("interval_secs is required for ScheduledTask")?;
    if interval_secs <= 0 {
        bail!("interval_secs must be greater than zero");
    }

    let add_fields = [
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(behavior_id)
        )),
        Some(format!(r#"name: "{}""#, escape_graphql_string(name))),
        Some(format!(r#"prompt: "{}""#, escape_graphql_string(prompt))),
        Some(graphql_optional_int_field(
            "interval_secs",
            Some(interval_secs),
        )),
        Some(graphql_optional_bool_field(
            "enabled",
            Some(row.enabled.unwrap_or(true)),
        )),
        Some(graphql_string_field(
            "next_run_at",
            row.next_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_run_at",
            row.last_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_status",
            row.last_status.as_deref(),
        )),
        Some(graphql_string_field(
            "last_error",
            row.last_error.as_deref(),
        )),
        Some(graphql_optional_int_field("run_count", row.run_count)),
    ];
    let update_fields = [
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(behavior_id)
        )),
        Some(format!(r#"name: "{}""#, escape_graphql_string(name))),
        Some(format!(r#"prompt: "{}""#, escape_graphql_string(prompt))),
        Some(graphql_optional_int_field(
            "interval_secs",
            Some(interval_secs),
        )),
        Some(graphql_optional_bool_field(
            "enabled",
            Some(row.enabled.unwrap_or(true)),
        )),
        Some(graphql_string_field(
            "next_run_at",
            row.next_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_run_at",
            row.last_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_status",
            row.last_status.as_deref(),
        )),
        Some(graphql_string_field(
            "last_error",
            row.last_error.as_deref(),
        )),
        Some(graphql_optional_int_field("run_count", row.run_count)),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_ScheduledTask(
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
    execute_mutation(node, &mutation, "upsert_scheduled_task").await
}

pub async fn run_scheduled_task_now(node: &EmbeddedNode, row: &ScheduledTaskRow) -> Result<()> {
    if row.enabled != Some(true) {
        bail!("scheduled task must be enabled before it can run now");
    }

    let mut triggered = row.clone();
    triggered.next_run_at = Some(Utc::now().to_rfc3339());
    upsert_scheduled_task(node, &triggered).await
}
