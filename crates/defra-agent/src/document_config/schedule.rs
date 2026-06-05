use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

/// Runtime-owned Schedule fields the trigger engine writes back after a fire
/// attempt.
///
/// Each field is optional so callers can update a subset — the helper only
/// emits GraphQL input entries for the fields that are `Some`, leaving
/// apply-owned fields (`enabled`, `interval_secs`, `cron`, `timezone`,
/// `missed_run_policy`, `task_id`, `concurrency`) untouched.
/// `fire_count_delta` expresses the desired increment (typically
/// `+1` on a successful fire); the helper performs a read-then-write because
/// DefraDB does not currently expose atomic increments. Racing writes may
/// undercount, which is acceptable for PR 1 (fire_count is bookkeeping, not a
/// correctness-critical counter).
#[derive(Debug, Default, Clone)]
pub(crate) struct ScheduleRuntimeUpdate {
    pub(crate) next_run_at: Option<String>,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) last_status: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) fire_count_delta: Option<i64>,
}

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

/// Update the runtime-owned fields on a `Schedule` document identified by its
/// apply-owned `schedule_id`.
///
/// Only writes fields present in `updates`; apply-owned fields (`enabled`,
/// `interval_secs`, `cron`, `timezone`, `missed_run_policy`, `task_id`,
/// `concurrency`) are never touched. Returns `Ok` even when the schedule doc
/// is missing — the caller is assumed to have
/// raced a delete from apply, which the reconcile path will resolve.
///
/// `fire_count_delta` triggers a read-then-write: the current `fire_count` is
/// loaded, the delta added, and the new value written. DefraDB does not
/// expose atomic increments today, so racing concurrent updates may
/// undercount; this is acceptable for the Schedule `fire_count` field per the
/// event-driven-tasks PR 1 plan.
pub(crate) async fn update_schedule_runtime_fields(
    node: &EmbeddedNode,
    schedule_id: &str,
    updates: ScheduleRuntimeUpdate,
) -> Result<()> {
    // Short-circuit: nothing to write.
    if updates.next_run_at.is_none()
        && updates.last_attempt_at.is_none()
        && updates.last_status.is_none()
        && updates.last_error.is_none()
        && updates.fire_count_delta.is_none()
    {
        return Ok(());
    }

    // Resolve the current fire_count if we need to increment it. Also use this
    // to detect whether the schedule doc still exists (idempotent behavior on
    // a deleted schedule).
    let current_fire_count = if updates.fire_count_delta.is_some() {
        let escaped_schedule_id = escape_graphql_string(schedule_id);
        let query = format!(
            r#"{{
                Schedule(
                    filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                    limit: 1
                ) {{
                    fire_count
                }}
            }}"#
        );
        let resp = node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!(
                "query Schedule fire_count for runtime update failed: {:?}",
                resp.errors
            );
        }
        let rows = resp
            .data
            .as_ref()
            .and_then(|data| data.get("Schedule"))
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            // Schedule doc disappeared; nothing to update.
            tracing::info!(
                schedule_id,
                "Schedule doc missing during runtime update; skipping"
            );
            return Ok(());
        }
        rows.first()
            .and_then(|row| row.get("fire_count"))
            .and_then(|value| value.as_i64())
            .unwrap_or(0)
    } else {
        0
    };

    // Build the input literal with only the requested fields so apply-owned
    // fields are never overwritten.
    let mut entries: Vec<String> = Vec::new();
    if let Some(v) = updates.next_run_at.as_ref() {
        entries.push(format!("next_run_at: \"{}\"", escape_graphql_string(v)));
    }
    if let Some(v) = updates.last_attempt_at.as_ref() {
        entries.push(format!("last_attempt_at: \"{}\"", escape_graphql_string(v)));
    }
    if let Some(v) = updates.last_status.as_ref() {
        entries.push(format!("last_status: \"{}\"", escape_graphql_string(v)));
    }
    if let Some(v) = updates.last_error.as_ref() {
        entries.push(format!("last_error: \"{}\"", escape_graphql_string(v)));
    }
    if let Some(delta) = updates.fire_count_delta {
        let new_fire_count = current_fire_count.saturating_add(delta);
        entries.push(format!("fire_count: {new_fire_count}"));
    }
    let input_literal = format!("{{ {} }}", entries.join(", "));

    let escaped_schedule_id = escape_graphql_string(schedule_id);
    // Use a filter-based mutation so we key on the apply-owned schedule_id and
    // don't need to resolve the _docID separately. DefraDB matches at most one
    // schedule (schedule_id is unique) so this updates the single target doc.
    let mutation = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                input: {input_literal}
            ) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "update Schedule runtime fields for {schedule_id} failed: {:?}",
            resp.errors
        );
    }

    Ok(())
}

/// Description of a scheduled trigger for a task.
///
/// Mirrors the `Schedule` GraphQL schema in
/// `crates/defra-agent-schemas/schemas/agent/schedule.graphql`. Includes both
/// apply-owned fields (`schedule_id`, `task_id`, `interval_secs`, `cron`,
/// `timezone`, `missed_run_policy`, `enabled`, `concurrency`, `created_at`,
/// `updated_at`) and runtime-owned fields
/// (`next_run_at`, `last_attempt_at`, `last_status`, `last_error`,
/// `fire_count`) because `DocumentRuntimeView` is a DB-read view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub schedule_id: String,
    pub task_id: Option<String>,
    pub interval_secs: Option<i64>,
    pub cron: Option<String>,
    pub timezone: Option<String>,
    pub missed_run_policy: Option<String>,
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
pub(crate) async fn list_schedule_records(node: &EmbeddedNode) -> Result<Vec<(String, Schedule)>> {
    let query = r#"{
            Schedule(order: { schedule_id: ASC }) {
                _docID
                schedule_id
                task_id
                interval_secs
                cron
                timezone
                missed_run_policy
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
                cron
                timezone
                missed_run_policy
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
