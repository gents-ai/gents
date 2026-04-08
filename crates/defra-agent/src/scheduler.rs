use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use rig::client::CompletionClient;
use rig::completion::Prompt;
use tokio_util::sync::CancellationToken;

use crate::backend_registry::{self, BackendPermit, BackendTracker};
use crate::config::ProfileConfig;
use crate::graphql::escape_graphql_string;
use crate::health_checker::ServiceHealthMap;
use crate::hook::{DefraSessionHook, FailurePolicy};
use crate::lifecycle::{ExecutionOrigin, RequestLifecycle};
use crate::mcp_pool::McpPool;
use crate::meta_tools::build_meta_tools;
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::session;
use crate::streaming::{DefraStreamWriter, StreamStatus, StreamWriter};
use crate::toolset::build_delegate_tool;

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
    pub profile_name: String,
    pub prompt: String,
    pub interval_secs: i64,
    pub enabled: bool,
    pub next_run_at: Option<chrono::DateTime<Utc>>,
    pub run_count: i64,
}

impl ScheduledTask {
    fn from_value(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            doc_id: v.get("_docID")?.as_str()?.to_string(),
            task_id: v.get("task_id")?.as_str()?.to_string(),
            name: v.get("name")?.as_str()?.to_string(),
            profile_name: v.get("profile_name")?.as_str()?.to_string(),
            prompt: v.get("prompt")?.as_str()?.to_string(),
            interval_secs: v.get("interval_secs")?.as_i64()?,
            enabled: v.get("enabled")?.as_bool()?,
            next_run_at: v
                .get("next_run_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
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

pub struct Scheduler {
    node: Arc<EmbeddedNode>,
    profiles: Vec<Arc<ProfileConfig>>,
    mcp_pool: McpPool,
    health_map: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    ops_graphql_endpoint: String,
    backend_tracker: Arc<BackendTracker>,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node: Arc<EmbeddedNode>,
        profiles: Vec<Arc<ProfileConfig>>,
        mcp_pool: McpPool,
        health_map: ServiceHealthMap,
        local_hostname: String,
        local_subnet: Option<String>,
        ops_graphql_endpoint: String,
        backend_tracker: Arc<BackendTracker>,
    ) -> Self {
        Self {
            node,
            profiles,
            mcp_pool,
            health_map,
            local_hostname,
            local_subnet,
            ops_graphql_endpoint,
            backend_tracker,
        }
    }
}
