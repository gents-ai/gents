use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

/// Load the runtime-owned `next_run_at` field for a single `Schedule` by its
/// apply-owned `schedule_id`.
///
/// The schedule snapshot carries a stale copy of `next_run_at` (captured at
/// reconcile time). The trigger engine's `ScheduleSource` needs the fresh
/// value on every tick to decide whether a schedule is due, so this query
/// projects only that one field to keep each tick cheap.
///
/// Returns:
/// * `Ok(Some(next_run_at))` — schedule exists and has a non-null
///   `next_run_at`. The string is the raw ISO-8601 timestamp as persisted.
/// * `Ok(None)` — either the schedule doc doesn't exist or `next_run_at` is
///   null (never scheduled a first fire). Callers treat both as "not due".
pub(crate) async fn load_schedule_next_run_at(
    node: &EmbeddedNode,
    schedule_id: &str,
) -> Result<Option<String>> {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let query = format!(
        r#"{{
            Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                limit: 1
            ) {{
                next_run_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query Schedule next_run_at failed: {:?}", resp.errors);
    }

    let next_run_at = resp
        .data
        .as_ref()
        .and_then(|data| data.get("Schedule"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("next_run_at"))
        .and_then(|value| value.as_str())
        .map(|s| s.to_string());

    Ok(next_run_at)
}

/// Description of a scheduled trigger for a task.
///
/// Mirrors the `Schedule` GraphQL schema in
/// `crates/defra-agent-protocol/schemas/agent/schedule.graphql`. Includes both
/// apply-owned fields (`schedule_id`, `task_id`, `interval_secs`, `enabled`,
/// `concurrency`, `created_at`, `updated_at`) and runtime-owned fields
/// (`next_run_at`, `last_attempt_at`, `last_status`, `last_error`,
/// `fire_count`) because `DocumentRuntimeView` is a DB-read view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub schedule_id: String,
    pub task_id: Option<String>,
    pub interval_secs: Option<i64>,
    pub enabled: bool,
    pub concurrency: Option<String>,
    pub next_run_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// List every `Schedule` document in the node, returning `(doc_id, schedule)`
/// pairs.
///
/// Schedules are addressed by a globally unique `schedule_id` (see
/// `schedule.graphql`), so this helper is not scoped by `agent_did`.
pub(crate) async fn list_schedule_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, Schedule)>> {
    let query = r#"{
            Schedule(order: { schedule_id: ASC }) {
                _docID
                schedule_id
                task_id
                interval_secs
                enabled
                concurrency
                next_run_at
                last_attempt_at
                last_status
                last_error
                fire_count
                created_at
                updated_at
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list Schedule failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "Schedule"))
}

/// Load a single `Schedule` document by its DefraDB `_docID`.
///
/// Used by the control watcher's update-dispatch path to classify an updated
/// document by collection when only the `_docID` is known.
pub(crate) async fn load_schedule_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, Schedule)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            Schedule(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                schedule_id
                task_id
                interval_secs
                enabled
                concurrency
                next_run_at
                last_attempt_at
                last_status
                last_error
                fire_count
                created_at
                updated_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query Schedule by _docID failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "Schedule"))
}
