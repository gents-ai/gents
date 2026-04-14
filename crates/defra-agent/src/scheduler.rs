use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use rig::completion::Prompt;
use tokio_util::sync::CancellationToken;

use crate::backend_registry::{self, BackendPermit, BackendTracker};
use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::hook::{DefraSessionHook, FailurePolicy};
use crate::lifecycle::{ExecutionOrigin, RequestLifecycle};
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::runtime_snapshot::{refresh_active_snapshot, ActiveRuntimeSnapshot};
use crate::session;
use crate::streaming::{DefraStreamWriter, StreamStatus, StreamWriter};
use crate::tool_surface::ToolRuntimeContext;

mod execution;
mod loop_impl;
#[cfg(test)]
mod tests;

const TICK_INTERVAL_SECS: u64 = 60;
const TASK_TIMEOUT_SECS: u64 = 900;
const BACKEND_WAIT_POLL_MS: u64 = 1_000;
const SCHEDULED_TASK_COLLECTION_MISSING_ERROR: &str =
    "Cannot query collection 'ScheduledTask': collection not found";

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScheduledTaskDocumentState {
    Missing,
    Deleted,
    LiveSameDoc,
    LiveDifferentDoc(String),
}

#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub doc_id: String,
    pub task_id: String,
    pub name: String,
    pub behavior_id: String,
    pub prompt: String,
    pub interval_secs: i64,
    pub enabled: bool,
    pub next_run_at: Option<chrono::DateTime<Utc>>,
    pub run_count: i64,
}

impl ScheduledTask {
    fn from_value(v: &serde_json::Value) -> Result<Self> {
        let behavior_id = required_string_field(v, "behavior_id")?.to_string();
        Ok(Self {
            doc_id: required_string_field(v, "_docID")?.to_string(),
            task_id: required_string_field(v, "task_id")?.to_string(),
            name: required_string_field(v, "name")?.to_string(),
            behavior_id,
            prompt: required_string_field(v, "prompt")?.to_string(),
            interval_secs: required_i64_field(v, "interval_secs")?,
            enabled: required_bool_field(v, "enabled")?,
            next_run_at: optional_rfc3339_field(v, "next_run_at")?,
            run_count: v.get("run_count").and_then(|v| v.as_i64()).unwrap_or(0),
        })
    }

    fn is_due(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.next_run_at {
            None => true,
            Some(next) => Utc::now() >= next,
        }
    }
}

fn required_string_field<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("ScheduledTask missing required string field '{field}'"))
}

fn required_i64_field(value: &serde_json::Value, field: &str) -> Result<i64> {
    value
        .get(field)
        .and_then(|value| value.as_i64())
        .ok_or_else(|| anyhow!("ScheduledTask missing required integer field '{field}'"))
}

fn required_bool_field(value: &serde_json::Value, field: &str) -> Result<bool> {
    value
        .get(field)
        .and_then(|value| value.as_bool())
        .ok_or_else(|| anyhow!("ScheduledTask missing required boolean field '{field}'"))
}

fn optional_rfc3339_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<Option<chrono::DateTime<Utc>>> {
    match value.get(field).and_then(|value| value.as_str()) {
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|value| Some(value.with_timezone(&Utc)))
            .map_err(|error| {
                anyhow!("ScheduledTask field '{field}' is not valid RFC3339: {error}")
            }),
        None => Ok(None),
    }
}

fn is_missing_scheduled_task_collection_error(error_text: &str) -> bool {
    error_text.contains(SCHEDULED_TASK_COLLECTION_MISSING_ERROR)
}

async fn lookup_task_document_state(
    node: &Arc<EmbeddedNode>,
    task: &ScheduledTask,
) -> Result<ScheduledTaskDocumentState> {
    let query = format!(
        r#"query {{
            ScheduledTask(
                showDeleted: true,
                filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                limit: 16
            ) {{
                _docID
                _deleted
            }}
        }}"#,
        task_id = escape_graphql_string(&task.task_id),
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        let error_text = format!("{:?}", response.errors);
        if is_missing_scheduled_task_collection_error(&error_text) {
            return Ok(ScheduledTaskDocumentState::Missing);
        }
        anyhow::bail!(
            "failed to inspect scheduled task '{}' document state: {:?}",
            task.name,
            response.errors
        );
    }

    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("ScheduledTask"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    if rows.is_empty() {
        return Ok(ScheduledTaskDocumentState::Missing);
    }

    let mut live_doc_ids = Vec::new();
    for row in &rows {
        let doc_id = row
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("ScheduledTask document state row missing _docID: {row}"))?;
        let deleted = row
            .get("_deleted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if !deleted {
            live_doc_ids.push(doc_id);
        }
    }

    if live_doc_ids.len() > 1 {
        anyhow::bail!(
            "multiple live ScheduledTask documents share task_id '{}'",
            task.task_id
        );
    }

    if let Some(doc_id) = live_doc_ids.first().copied() {
        if doc_id == task.doc_id {
            Ok(ScheduledTaskDocumentState::LiveSameDoc)
        } else {
            Ok(ScheduledTaskDocumentState::LiveDifferentDoc(
                doc_id.to_string(),
            ))
        }
    } else {
        Ok(ScheduledTaskDocumentState::Deleted)
    }
}

async fn update_task_runtime_state(
    node: &Arc<EmbeddedNode>,
    task: &ScheduledTask,
    last_status: &str,
    last_error: Option<&str>,
) -> Result<()> {
    let now = Utc::now();
    let next_run = now + chrono::Duration::seconds(task.interval_secs);
    let now_str = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let next_str = next_run.to_rfc3339_opts(SecondsFormat::Secs, true);
    let new_run_count = task.run_count + 1;

    let mutation = format!(
        r#"mutation {{
            update_ScheduledTask(
                docID: "{doc_id}",
                input: {{
                    last_status: "{last_status}",
                    last_run_at: "{last_run}",
                    next_run_at: "{next_run}",
                    run_count: {count},
                    last_error: "{last_error}"
                }}
            ) {{ _docID }}
        }}"#,
        doc_id = task.doc_id,
        last_status = escape_graphql_string(last_status),
        last_run = now_str,
        next_run = next_str,
        count = new_run_count,
        last_error = escape_graphql_string(last_error.unwrap_or("")),
    );

    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "failed to update task '{}' {} bookkeeping: {:?}",
            task.name,
            last_status,
            response.errors
        );
    }

    if response
        .data
        .as_ref()
        .and_then(|data| data.get("update_ScheduledTask"))
        .is_some_and(response_has_documents)
    {
        return Ok(());
    }

    match lookup_task_document_state(node, task).await? {
        ScheduledTaskDocumentState::Missing => {
            tracing::info!(
                task_id = %task.task_id,
                doc_id = %task.doc_id,
                last_status,
                "scheduled task doc missing before runtime bookkeeping update; skipping stale update"
            );
            Ok(())
        }
        ScheduledTaskDocumentState::Deleted => {
            tracing::info!(
                task_id = %task.task_id,
                doc_id = %task.doc_id,
                last_status,
                "scheduled task doc deleted before runtime bookkeeping update; skipping stale update"
            );
            Ok(())
        }
        ScheduledTaskDocumentState::LiveDifferentDoc(current_doc_id) => {
            tracing::info!(
                task_id = %task.task_id,
                stale_doc_id = %task.doc_id,
                current_doc_id,
                last_status,
                "scheduled task doc was superseded before runtime bookkeeping update; skipping stale update"
            );
            Ok(())
        }
        ScheduledTaskDocumentState::LiveSameDoc => anyhow::bail!(
            "task '{}' {} bookkeeping mutation returned no document for live doc {}",
            task.name,
            last_status,
            task.doc_id
        ),
    }
}

pub struct Scheduler {
    node: Arc<EmbeddedNode>,
    active_snapshot: Arc<ActiveRuntimeSnapshot>,
    active_snapshot_rx: tokio::sync::watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    tool_runtime: ToolRuntimeContext,
    backend_tracker: Arc<BackendTracker>,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        active_snapshot_rx: tokio::sync::watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        tool_runtime: ToolRuntimeContext,
        backend_tracker: Arc<BackendTracker>,
    ) -> Self {
        let active_snapshot = active_snapshot_rx.borrow().clone();
        Self {
            node,
            active_snapshot,
            active_snapshot_rx,
            tool_runtime,
            backend_tracker,
        }
    }

    fn current_snapshot(&mut self) -> Arc<ActiveRuntimeSnapshot> {
        refresh_active_snapshot(&mut self.active_snapshot, &mut self.active_snapshot_rx);
        self.active_snapshot.clone()
    }
}
