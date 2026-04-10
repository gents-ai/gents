use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session;
use crate::watcher::AgentRequest;

mod claim;
mod lookup;
mod materialize;
mod query;
mod recovery;
mod rows;
mod transition;

pub const DEFAULT_REQUEST_MAX_RETRIES: u32 = 3;

fn graphql_retry_root_request(retry_root_request: Option<&str>, request_id: &str) -> String {
    escape_graphql_string(retry_root_request.unwrap_or(request_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalLifecycleState {
    Pending,
    Claimed,
    Streaming,
    Completed,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOrigin {
    Interactive,
    Scheduled,
}

impl ExecutionOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Scheduled => "scheduled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum PersistedLifecycleState {
    Pending,
    Claimed,
    Processing,
    InputRequired,
    Completed,
    Failed,
    Superseded,
    Dead,
}

impl PersistedLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Processing => "processing",
            Self::InputRequired => "inputRequired",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
            Self::Dead => "dead",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistedAdmissionState {
    Released,
    Waiting,
    Acquired,
    Executing,
}

impl PersistedAdmissionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::Waiting => "waiting",
            Self::Acquired => "acquired",
            Self::Executing => "executing",
        }
    }
}

pub struct RequestLifecycle {
    node: Arc<EmbeddedNode>,
    agent_name: String,
    agent_did: String,
    behavior_id: String,
    execution_origin: ExecutionOrigin,
    backend_id: String,
    failure_reason: Option<String>,
    request: AgentRequest,
    response_doc_id: Option<String>,
    progress_seq: u32,
    deadline_duration_secs: u64,
    state: LocalLifecycleState,
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub requests_recovered: usize,
    pub responses_recovered: usize,
    pub conversations_recovered: usize,
}

fn resolve_behavior_id(default_behavior_id: &str, requested_behavior_id: Option<&str>) -> String {
    requested_behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_behavior_id)
        .to_string()
}
