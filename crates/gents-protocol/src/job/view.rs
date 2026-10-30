use serde::{Deserialize, Serialize};

use super::{JobChildState, JobControlKind, JobOrigin, JobProgress, JobState, JobUsage};

/// The request a job row belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobScope {
    pub session_id: String,
    pub request_id: String,
    pub node_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub requester_did: Option<String>,
}

/// The terminal output's code and message: a host `plugin_job_*` code or the
/// engine's own.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobOutcome {
    pub code: String,
    pub message: String,
}

/// The pin fields a client needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobPinSummary {
    pub script_id: String,
    pub origin: JobOrigin,
    pub engine_digest: String,
    pub plugin_ref: String,
    pub agent_budget: u32,
    pub cwd: String,
    pub chain_root_doc_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub resume_of: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobChildRow {
    pub slot: String,
    pub attempt: u32,
    pub session_id: String,
    pub request_id: String,
    pub state: JobChildState,
    pub title: String,
    pub usage: JobUsage,
    pub duration_ms: u64,
    pub target: String,
}

/// A client's view of one job row.
///
/// `address` is the job key `session_id:tool_call_id`, split at the last `':'`
/// (tool-call ids contain no `':'`). It is not named `key` because
/// `strip_secret_fields` drops a field whose name contains KEY.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobView {
    pub address: String,
    pub row_doc_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub scope: JobScope,
    pub handle: String,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub outcome: Option<JobOutcome>,
    pub pin: JobPinSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub progress: Option<JobProgress>,
    pub children: Vec<JobChildRow>,
    /// Unix epoch milliseconds on the host clock, as are `adopted_at` and
    /// `completed_at`.
    pub created_at: u64,
    pub adopted_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub completed_at: Option<u64>,
}

pub const JOB_CONTROL_PATH: &str = "/jobs/control";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct OperatorJobControlRequest {
    pub node_did: String,
    pub tool_call_doc_id: String,
    pub action: JobControlKind,
}

/// The result of controlling a job row. Control is local-only: it runs on
/// the runtime that drives the row, while the view also works remotely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobControlOutcome {
    Applied {
        state: JobState,
        killed_children: u32,
    },
    AlreadyTerminal {
        state: JobState,
    },
}

/// Why a job row could not be read. Transient errors are retried by a cursor;
/// permanent ones are surfaced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobReadError {
    NotAJob,
    NotAuthorized,
    NotYetObservable,
    Unavailable { message: String },
    Malformed { message: String },
}

impl JobReadError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::NotYetObservable | Self::Unavailable { .. })
    }
}

impl std::fmt::Display for JobReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAJob => {
                f.write_str("not a plugin job; find the job's address with `gents job list`")
            }
            Self::NotAuthorized => f.write_str(
                "this job belongs to another session or requester; read it from the session that started it",
            ),
            Self::NotYetObservable => f.write_str(
                "no job row is visible at this address yet; retry shortly, or check the address with `gents job list`",
            ),
            Self::Unavailable { message } => write!(f, "job store unavailable: {message}"),
            Self::Malformed { message } => {
                write!(f, "job row does not decode: {message}; retrying will not help")
            }
        }
    }
}

impl std::error::Error for JobReadError {}
