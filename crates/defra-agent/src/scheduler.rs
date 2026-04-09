use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use rig::client::CompletionClient;
use rig::completion::Prompt;
use tokio_util::sync::CancellationToken;

use crate::backend_registry::{self, BackendPermit, BackendTracker};
use crate::graphql::escape_graphql_string;
use crate::hook::{DefraSessionHook, FailurePolicy};
use crate::lifecycle::{ExecutionOrigin, RequestLifecycle};
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::runtime_snapshot::{refresh_active_snapshot, ActiveRuntimeSnapshot};
use crate::session;
use crate::streaming::{DefraStreamWriter, StreamStatus, StreamWriter};
use crate::tool_surface::ToolRuntimeContext;

mod execution;
mod loop_impl;
mod ops;
#[cfg(test)]
mod tests;

const TICK_INTERVAL_SECS: u64 = 60;
const TASK_TIMEOUT_SECS: u64 = 900;
const BACKEND_WAIT_POLL_MS: u64 = 1_000;

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

pub struct Scheduler {
    node: Arc<EmbeddedNode>,
    active_snapshot: Arc<ActiveRuntimeSnapshot>,
    active_snapshot_rx: tokio::sync::watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    tool_runtime: ToolRuntimeContext,
    ops_graphql_endpoint: String,
    backend_tracker: Arc<BackendTracker>,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        active_snapshot_rx: tokio::sync::watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        tool_runtime: ToolRuntimeContext,
        ops_graphql_endpoint: String,
        backend_tracker: Arc<BackendTracker>,
    ) -> Self {
        let active_snapshot = active_snapshot_rx.borrow().clone();
        Self {
            node,
            active_snapshot,
            active_snapshot_rx,
            tool_runtime,
            ops_graphql_endpoint,
            backend_tracker,
        }
    }

    fn current_snapshot(&mut self) -> Arc<ActiveRuntimeSnapshot> {
        refresh_active_snapshot(&mut self.active_snapshot, &mut self.active_snapshot_rx);
        self.active_snapshot.clone()
    }
}
