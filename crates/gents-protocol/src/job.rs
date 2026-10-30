use serde::{Deserialize, Serialize};

pub mod keys;
mod round;
mod settings;
mod step;
mod step_output;
mod view;

pub use round::{
    ChildOutputRequest, ChildOutputResponse, ChildOutputSlice, DataCall, DataQueryResponse,
    JobCall, JobResultRequest, JobResultResponse, RoundResult,
};
pub use settings::{
    JobPolicy, PluginDataDeclaration, PluginJobDeclaration, PluginJobSettings, StampedField,
};
pub use step::{
    CALLER_NODE_DID_FILTER, DataOp, DataQuery, DataWrite, JobControlKind, JobControlRequest,
    JobEffects, JobFailure, JobInterrupt, JobMessage, JobScratch, JobSpawn, JobStepInput,
    JobSuspend, JobSuspendKind, OnConflict,
};
pub use step_output::JobStepOutput;
pub use view::{
    JOB_CONTROL_PATH, JobChildRow, JobControlOutcome, JobOutcome, JobPinSummary, JobReadError,
    JobScope, JobView, OperatorJobControlRequest,
};

// No field below may be named with KEY, SECRET, TOKEN or PASSWORD in any case:
// `strip_secret_fields` drops such keys from callback documents, and the step
// driver builds this input natively.

/// Row-level job state. Mirrors Lean `PluginJob.JobState`; a paused or awaiting
/// row is distinct from a completed one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobState {
    Running,
    Paused,
    AwaitingUser,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

/// State of one spawned child request. Mirrors Lean `PluginJob.ChildState`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobChildState {
    Live,
    Completed,
    Failed,
    Interrupted,
    Other,
}

/// Why a job paused; shared by a suspend output and `JobProgress.pause_reason`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobPauseReason {
    User,
    BackOff,
    NoProgress,
    Verification,
    Infra,
    BudgetExceeded,
}

/// Host fact: whether the call carries a model-authored `script` (dynamic) or
/// runs the plugin artifact (installed).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobOrigin {
    Installed,
    Dynamic,
}

/// How dynamic (non-installed) scripts are treated; mirrors Lean `PluginJob.DynamicMode`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum DynamicMode {
    Off,
    Check,
    Run,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobPhaseState {
    Pending,
    Active,
    Done,
    Failed,
}

/// Precedence is answer, then engine_current, then resume.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobControlEchoKind {
    Resume,
    Answer,
    EngineCurrent,
}

/// The only engine selector a resume block may carry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobResumeEngine {
    Current,
}

/// Size cap of a serialized `JobProgress`.
pub const JOB_PROGRESS_MAX_BYTES: usize = 16 * 1024;
/// The cap on a guest's own step state (`JobStepInput.state`, `JobEffects.state`), which the
/// host stores verbatim and never reads.
pub const JOB_GUEST_STATE_MAX_BYTES: usize = 64 * 1024;
/// Size cap of one `JobProgress` title or event string.
pub const JOB_PROGRESS_TEXT_MAX_BYTES: usize = 256;

/// Written once, in the adoption transaction. The host builds the whole pin from
/// the guest's `JobGuestPin` (`script_id`, `source_digest`, `agent_budget`,
/// `handle`) plus host facts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobPin {
    pub script_id: String,
    pub source_digest: String,
    pub origin: JobOrigin,
    pub engine_digest: String,
    pub plugin_ref: String,
    pub agent_budget: u32,
    pub handle: String,
    pub cwd: String,
    /// Unix epoch milliseconds on the host clock.
    pub adopted_at: u64,
    pub args_digest: String,
    pub chain_root_doc_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub resume_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub replayed_from: Option<String>,
    pub ceiling_at_pin: JobCeiling,
}

/// The pin fields a guest step output may carry; mirrors Lean
/// `PluginJob.GuestPin`. The host builds the stored `JobPin` from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobGuestPin {
    pub script_id: String,
    pub source_digest: String,
    pub agent_budget: u32,
    pub handle: String,
}

/// The job ceiling recorded in the pin; mirrors Lean `PluginJob.Ceiling`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobCeiling {
    pub targets: Vec<String>,
    pub max_live: u32,
    pub max_spawns: u32,
    pub max_messages_per_slot: u32,
    pub result_bytes: u64,
    pub max_wall_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobPhase {
    pub title: String,
    pub state: JobPhaseState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobActiveChild {
    pub slot: String,
    pub title: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobBudget {
    pub total: u32,
    pub spent: u32,
    pub remaining: u32,
}

/// Last-writer-wins progress of one job row. At most `JOB_PROGRESS_MAX_BYTES`
/// serialized; titles and events at most `JOB_PROGRESS_TEXT_MAX_BYTES`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobProgress {
    pub title: String,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub pause_reason: Option<JobPauseReason>,
    pub phases: Vec<JobPhase>,
    pub active: Vec<JobActiveChild>,
    pub spawned: u32,
    pub budget: JobBudget,
    /// On the adopting output this carries the resolved tier and origin line.
    pub last_event: String,
    /// The guest may omit it; the host stamps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub last_event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub question: Option<String>,
    pub observed_terminal_children: u32,
    /// Host-owned; a guest-supplied value is ignored.
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobRef {
    pub row_doc_id: String,
    pub chain_root_doc_id: String,
    pub chain: Vec<String>,
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobCaller {
    pub session_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub tool_call_id: String,
    pub node_did: String,
    pub agent_id: String,
    pub request_hop: u32,
    pub correlation: Option<String>,
    pub cwd: String,
    pub started_by_job: bool,
}

/// A validated resume's chain-root arguments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobRootCall {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "Record<string, unknown> | null", optional = nullable))]
    pub args: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub agent_budget: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobCeilingView {
    pub targets: Vec<String>,
    pub max_live: u32,
    pub remaining_spawns: u32,
    pub max_messages_per_slot: u32,
    pub result_bytes: u64,
    pub dynamic: DynamicMode,
    pub dynamic_targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobChildView {
    pub attempt: u32,
    pub session_id: String,
    pub request_id: String,
    pub title: String,
    pub state: JobChildState,
    pub output_bytes: u64,
    pub truncated: bool,
    pub usage: JobUsage,
    pub duration_ms: u64,
}

/// The step-0 resume block, host-validated before it is echoed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobResume {
    pub row_doc_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub engine: Option<JobResumeEngine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub budget: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobControlEcho {
    pub kind: JobControlEchoKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub payload: Option<JobResume>,
}

/// One row of the caller session's jobs for this plugin. `JobStepInput.jobs`
/// lists them oldest first by `adopted_at`, so a run's resumes share its
/// `handle` and its newest row is the last listed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobRowView {
    pub row_doc_id: String,
    pub tool_call_id: String,
    pub handle: String,
    pub state: JobState,
    /// The pin's `adopted_at`.
    pub adopted_at: u64,
    /// The pin's `resume_of`: the row this one resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub resume_of: Option<String>,
}

#[cfg(test)]
mod tests;
