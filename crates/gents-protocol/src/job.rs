use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod keys;

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

/// Host fact: whether the script is a file in a trusted pack mount.
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
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub agent_budget: Option<u32>,
}

/// The verified bytes the engine must run (installed origin, steps after the first).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobVerifiedScript {
    pub source: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AssetMount {
    pub pack: String,
    pub class: String,
    pub digest: String,
    pub path: String,
    /// Host hint: operator provenance and listed in `trusted_packs`.
    pub trusted: bool,
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

/// The input of one job step, built natively by the step driver. Every field is
/// host-owned except the echoed arguments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobStepInput {
    /// Always 1.
    pub protocol: u32,
    pub job: JobRef,
    pub caller: JobCaller,
    /// Every model-facing argument below is echoed verbatim.
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "Record<string, unknown> | null", optional = nullable))]
    pub args: Option<serde_json::Map<String, serde_json::Value>>,
    /// Binds on step 0 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub path: Option<String>,
    /// The model's script text, never the verified bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub agent_budget: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub root_call: Option<JobRootCall>,
    /// Null only on step 0.
    pub pin: Option<JobPin>,
    /// Null only on step 0.
    pub origin: Option<JobOrigin>,
    /// Host-verified bytes; separate from `script`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub verified_script: Option<JobVerifiedScript>,
    pub mounts: Vec<AssetMount>,
    pub ceiling: JobCeilingView,
    pub children: BTreeMap<String, JobChildView>,
    pub control: Option<JobControlEcho>,
    pub scratch: BTreeMap<String, String>,
    pub jobs: Vec<JobRowView>,
    /// The guest's own state from its last step that carried one, stored verbatim and never
    /// read by the host; absent before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "unknown | null", optional = nullable))]
    pub state: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobSuspendKind {
    Pause,
    AwaitUser,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobSuspend {
    pub kind: JobSuspendKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub reason: Option<JobPauseReason>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub question: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobControlKind {
    Stop,
    Pause,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobControlRequest {
    pub row_doc_id: String,
    pub kind: JobControlKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobSpawn {
    pub slot: String,
    pub attempt: u32,
    pub target: String,
    pub prompt: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobMessage {
    pub slot: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobInterrupt {
    pub slot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobScratch {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum DataOp {
    Insert,
    Update,
    Delete,
}

/// What a write does when the document already exists.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum OnConflict {
    Ignore,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DataWrite {
    pub op: DataOp,
    pub collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub doc_id: Option<String>,
    #[cfg_attr(feature = "typescript", ts(type = "Record<string, unknown>"))]
    pub fields: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub on_conflict: Option<OnConflict>,
}

/// A filter value the host replaces with the calling node's DID.
pub const CALLER_NODE_DID_FILTER: &str = "$caller.node_did";

/// A read in the actor's ACP view. The host bounds `limit` at 256; nothing
/// bounds `offset`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DataQuery {
    pub collection: String,
    #[cfg_attr(feature = "typescript", ts(type = "Record<string, unknown>"))]
    pub filter: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "Record<string, unknown> | null", optional = nullable))]
    pub order: Option<serde_json::Map<String, serde_json::Value>>,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub offset: Option<u32>,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobEffects {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub pin: Option<JobGuestPin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub resume: Option<JobResume>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<JobSpawn>>", optional = nullable))]
    pub spawn: Vec<JobSpawn>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<JobMessage>>", optional = nullable))]
    pub message: Vec<JobMessage>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<JobInterrupt>>", optional = nullable))]
    pub interrupt: Vec<JobInterrupt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub progress: Option<JobProgress>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<DataWrite>>", optional = nullable))]
    pub writes: Vec<DataWrite>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<JobScratch>>", optional = nullable))]
    pub scratch: Vec<JobScratch>,
    /// The guest's own state for its next step, stored verbatim; absent keeps the stored one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(type = "unknown | null", optional = nullable))]
    pub state: Option<serde_json::Value>,
}

/// The output of one job step: exactly one of `result`, `fail`, `suspend` or
/// `effects`, plus that variant's sibling riders. Serde's external tagging
/// cannot carry the riders, so `Deserialize` and `Serialize` are hand-written.
///
/// `pin` on `Result` or `Suspend` is accepted on step 0 only, and `resume` only
/// beside it; the output is then delivered inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStepOutput {
    Result {
        result: serde_json::Value,
        writes: Vec<DataWrite>,
        scratch: Vec<JobScratch>,
        progress: Option<JobProgress>,
        control: Vec<JobControlRequest>,
        pin: Option<JobGuestPin>,
        resume: Option<JobResume>,
    },
    Fail {
        fail: JobFailure,
        writes: Vec<DataWrite>,
        progress: Option<JobProgress>,
    },
    Suspend {
        suspend: JobSuspend,
        writes: Vec<DataWrite>,
        scratch: Vec<JobScratch>,
        progress: Option<JobProgress>,
        pin: Option<JobGuestPin>,
        resume: Option<JobResume>,
        state: Option<serde_json::Value>,
    },
    Effects {
        effects: JobEffects,
    },
}

const STEP_OUTPUT_DISCRIMINATORS: [&str; 4] = ["result", "fail", "suspend", "effects"];

type StepObject = serde_json::Map<String, serde_json::Value>;

fn rider<T>(object: &mut StepObject, key: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned + Default,
{
    match object.remove(key) {
        None | Some(serde_json::Value::Null) => Ok(T::default()),
        Some(value) => serde_json::from_value(value).map_err(|e| format!("`{key}`: {e}")),
    }
}

fn required<T>(object: &mut StepObject, key: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let value = object.remove(key).unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value).map_err(|e| format!("`{key}`: {e}"))
}

fn step_output_variant(kind: &str, o: &mut StepObject) -> Result<JobStepOutput, String> {
    Ok(match kind {
        "result" => JobStepOutput::Result {
            result: o.remove("result").unwrap_or_default(),
            writes: rider(o, "writes")?,
            scratch: rider(o, "scratch")?,
            progress: rider(o, "progress")?,
            control: rider(o, "control")?,
            pin: rider(o, "pin")?,
            resume: rider(o, "resume")?,
        },
        "fail" => JobStepOutput::Fail {
            fail: required(o, "fail")?,
            writes: rider(o, "writes")?,
            progress: rider(o, "progress")?,
        },
        "suspend" => JobStepOutput::Suspend {
            suspend: required(o, "suspend")?,
            writes: rider(o, "writes")?,
            scratch: rider(o, "scratch")?,
            progress: rider(o, "progress")?,
            pin: rider(o, "pin")?,
            resume: rider(o, "resume")?,
            state: rider(o, "state")?,
        },
        _ => JobStepOutput::Effects {
            effects: required(o, "effects")?,
        },
    })
}

impl<'de> Deserialize<'de> for JobStepOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        const SHAPE: &str =
            "a JSON object with exactly one of `result`, `fail`, `suspend` or `effects`";
        let serde_json::Value::Object(mut object) = serde_json::Value::deserialize(deserializer)?
        else {
            return Err(D::Error::custom(format!("expected {SHAPE}")));
        };
        let present: Vec<&str> = STEP_OUTPUT_DISCRIMINATORS
            .into_iter()
            .filter(|key| object.contains_key(*key))
            .collect();
        let [kind] = present[..] else {
            return Err(D::Error::custom(format!(
                "expected {SHAPE}, found {} of them",
                present.len()
            )));
        };
        let riders: &[&str] = match kind {
            "result" => &["writes", "scratch", "progress", "control", "pin", "resume"],
            "fail" => &["writes", "progress"],
            "suspend" => &["writes", "scratch", "progress", "pin", "resume", "state"],
            _ => &[],
        };
        if let Some(unknown) = object
            .keys()
            .find(|key| key.as_str() != kind && !riders.contains(&key.as_str()))
        {
            return Err(D::Error::custom(format!(
                "unknown key `{unknown}` beside `{kind}`; expected {SHAPE}, and `{kind}` accepts only the riders {riders:?}"
            )));
        }
        step_output_variant(kind, &mut object).map_err(D::Error::custom)
    }
}

impl Serialize for JobStepOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        fn list<M, T>(m: &mut M, key: &str, v: &[T]) -> Result<(), M::Error>
        where
            M: SerializeMap,
            T: Serialize,
        {
            if v.is_empty() {
                Ok(())
            } else {
                m.serialize_entry(key, v)
            }
        }
        fn opt<M, T>(m: &mut M, key: &str, v: &Option<T>) -> Result<(), M::Error>
        where
            M: SerializeMap,
            T: Serialize,
        {
            match v {
                Some(v) => m.serialize_entry(key, v),
                None => Ok(()),
            }
        }
        let mut m = serializer.serialize_map(None)?;
        match self {
            Self::Result {
                result,
                writes,
                scratch,
                progress,
                control,
                pin,
                resume,
            } => {
                m.serialize_entry("result", result)?;
                list(&mut m, "writes", writes)?;
                list(&mut m, "scratch", scratch)?;
                opt(&mut m, "progress", progress)?;
                list(&mut m, "control", control)?;
                opt(&mut m, "pin", pin)?;
                opt(&mut m, "resume", resume)?;
            }
            Self::Fail {
                fail,
                writes,
                progress,
            } => {
                m.serialize_entry("fail", fail)?;
                list(&mut m, "writes", writes)?;
                opt(&mut m, "progress", progress)?;
            }
            Self::Suspend {
                suspend,
                writes,
                scratch,
                progress,
                pin,
                resume,
                state,
            } => {
                m.serialize_entry("suspend", suspend)?;
                list(&mut m, "writes", writes)?;
                list(&mut m, "scratch", scratch)?;
                opt(&mut m, "progress", progress)?;
                opt(&mut m, "pin", pin)?;
                opt(&mut m, "resume", resume)?;
                opt(&mut m, "state", state)?;
            }
            Self::Effects { effects } => m.serialize_entry("effects", effects)?,
        }
        m.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ChildOutputRequest {
    pub slots: Vec<String>,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ChildOutputResponse {
    pub slots: BTreeMap<String, ChildOutputSlice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobResultRequest {
    pub row_doc_id: String,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobResultResponse {
    pub row_doc_id: String,
    pub state: JobState,
    pub result: String,
    pub offset: u64,
    pub total_bytes: u64,
    pub truncated: bool,
}

/// One `data_calls` request; its answer is `data_results[id]`, a
/// `RoundResult<DataQueryResponse>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DataCall {
    pub id: String,
    pub query: DataQuery,
}

/// The rows a `data_calls.query` returns, each holding the requested `fields`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DataQueryResponse {
    #[cfg_attr(feature = "typescript", ts(type = "Array<Record<string, unknown>>"))]
    pub rows: Vec<serde_json::Map<String, serde_json::Value>>,
}

/// One `job_calls` request; its answer is `job_results[id]`, a `RoundResult`
/// carrying a `ChildOutputResponse` or a `JobResultResponse`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged, deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobCall {
    ChildOutput {
        id: String,
        child_output: ChildOutputRequest,
    },
    Result {
        id: String,
        result: JobResultRequest,
    },
}

/// The answer to one round request, keyed by the request's `id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged, deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum RoundResult<T> {
    Result { result: T },
    Error { error: String },
}

/// One requested slot's terminal text from `offset`, capped at `limit` and at
/// `result_bytes`; `truncated` when the text continues past this slice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ChildOutputSlice {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional))]
    pub truncated: bool,
}

/// Where a catalog entry was found.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum CatalogSource {
    Pack,
    Project,
    Kept,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct CatalogEntry {
    pub name: String,
    pub description: String,
    pub when_to_use: String,
    pub source: CatalogSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub path: Option<String>,
}

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

/// Operator-owned job ceiling of one plugin: what a job it runs may start and
/// how long it may live. An absent block means non-job verbs only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginJobSettings {
    /// AgentTarget ids the job may start children on; at least one.
    #[serde(default)]
    pub agent_targets: Vec<String>,
    /// Children alive at once across the job.
    #[serde(default = "PluginJobSettings::default_max_live")]
    pub max_live: u32,
    /// Children the job may start in total.
    #[serde(default = "PluginJobSettings::default_max_spawns")]
    pub max_spawns: u32,
    /// Messages one child slot may receive per attempt.
    #[serde(default = "PluginJobSettings::default_max_messages_per_slot")]
    pub max_messages_per_slot: u32,
    /// Bytes of the job's result.
    #[serde(default = "PluginJobSettings::default_result_bytes")]
    pub result_bytes: u64,
    /// Seconds the job may run.
    #[serde(default = "PluginJobSettings::default_max_wall_secs")]
    pub max_wall_secs: u64,
    /// Operator-only rules for dynamic scripts, trusted packs and project roots.
    #[serde(default)]
    pub policy: JobPolicy,
}

impl PluginJobSettings {
    pub const DEFAULT_MAX_LIVE: u32 = 8;
    pub const DEFAULT_MAX_SPAWNS: u32 = 128;
    pub const DEFAULT_MAX_MESSAGES_PER_SLOT: u32 = 4;
    pub const DEFAULT_RESULT_BYTES: u64 = 65_536;
    pub const DEFAULT_MAX_WALL_SECS: u64 = 7_200;
    pub const MAX_WALL_SECS_CAP: u64 = 86_400;

    fn default_max_live() -> u32 {
        Self::DEFAULT_MAX_LIVE
    }

    fn default_max_spawns() -> u32 {
        Self::DEFAULT_MAX_SPAWNS
    }

    fn default_max_messages_per_slot() -> u32 {
        Self::DEFAULT_MAX_MESSAGES_PER_SLOT
    }

    fn default_result_bytes() -> u64 {
        Self::DEFAULT_RESULT_BYTES
    }

    fn default_max_wall_secs() -> u64 {
        Self::DEFAULT_MAX_WALL_SECS
    }
}

/// Operator-only job policy; self-config may not edit it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobPolicy {
    #[serde(default = "JobPolicy::default_dynamic")]
    pub dynamic: DynamicMode,
    #[serde(default)]
    pub dynamic_targets: Vec<String>,
    #[serde(default)]
    pub trusted_packs: Vec<String>,
    #[serde(default)]
    pub project_roots: Vec<String>,
}

impl JobPolicy {
    pub const DEFAULT_DYNAMIC: DynamicMode = DynamicMode::Check;

    fn default_dynamic() -> DynamicMode {
        Self::DEFAULT_DYNAMIC
    }
}

impl Default for JobPolicy {
    fn default() -> Self {
        Self {
            dynamic: Self::DEFAULT_DYNAMIC,
            dynamic_targets: Vec::new(),
            trusted_packs: Vec::new(),
            project_roots: Vec::new(),
        }
    }
}

/// Manifest `job`: the plugin runs job steps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginJobDeclaration(pub bool);

/// The context value a stamped field is host-filled from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum StampedField {
    NodeDid,
    SessionId,
    RequestDocId,
    ToolCallDocId,
    JobRowDocId,
    ChainRootDocId,
}

/// Manifest `data`: `ops` are `query`, `insert`, `update` or `delete`; `stamped`
/// maps a written field to the context value that fills it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginDataDeclaration {
    pub collections: Vec<String>,
    pub ops: Vec<String>,
    #[serde(default)]
    pub stamped: BTreeMap<String, StampedField>,
}

/// Manifest `asset_mounts[]`: one asset class the plugin reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AssetMountDeclaration {
    pub class: String,
}

/// Manifest `catalog`: the argv of the plugin's catalog call. Catalog calls
/// use the raw-argument shape, so there is no catalog input type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginCatalogDeclaration {
    pub argv: Vec<String>,
}

/// Reserved input key under which non-job calls receive the asset mount list.
pub const ASSET_MOUNTS_INPUT_KEY: &str = "asset_mounts";

/// `AssetMount.class` of a project-tier mount.
pub const PROJECT_MOUNT_CLASS: &str = "project";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(value: serde_json::Value) -> Result<JobStepOutput, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn effects_of(output: JobStepOutput) -> JobEffects {
        match output {
            JobStepOutput::Effects { effects } => effects,
            other => panic!("expected an effects output, got {other:?}"),
        }
    }

    fn assert_lists_empty(e: &JobEffects) {
        assert!(e.spawn.is_empty() && e.message.is_empty() && e.interrupt.is_empty());
        assert!(e.writes.is_empty() && e.scratch.is_empty());
    }

    #[test]
    fn effects_lists_default_to_empty_when_absent() {
        let e = effects_of(decode(json!({ "effects": {} })).expect("decode"));
        assert_lists_empty(&e);
        assert_eq!((e.pin, e.resume, e.progress), (None, None, None));
    }

    #[test]
    fn effects_lists_default_to_empty_when_null() {
        let e = effects_of(
            decode(json!({ "effects": {
                "spawn": null, "message": null, "interrupt": null,
                "writes": null, "scratch": null
            } }))
            .expect("decode"),
        );
        assert_lists_empty(&e);
    }

    #[test]
    fn effects_decode_one_list_alone() {
        let e = effects_of(
            decode(json!({ "effects": { "interrupt": [{ "slot": "0:a" }] } })).expect("decode"),
        );
        assert_eq!(
            e.interrupt,
            vec![JobInterrupt {
                slot: "0:a".to_string()
            }]
        );
        assert!(e.spawn.is_empty() && e.message.is_empty());
        assert!(e.writes.is_empty() && e.scratch.is_empty());
    }

    #[test]
    fn empty_effects_lists_are_omitted_on_encode() {
        let output = decode(json!({ "effects": { "message": [], "writes": [] } })).expect("decode");
        assert_eq!(
            serde_json::to_value(&output).expect("encode"),
            json!({ "effects": {} })
        );
    }

    #[test]
    fn guest_state_rides_on_effects_and_suspend_only() {
        let effects = effects_of(decode(json!({"effects": {"state": {"phase": "scan"}}})).unwrap());
        assert_eq!(effects.state, Some(json!({"phase": "scan"})));
        let suspend =
            decode(json!({"suspend": {"kind": "pause", "message": "m"}, "state": [1, 2]})).unwrap();
        assert!(
            matches!(&suspend, JobStepOutput::Suspend { state, .. } if *state == Some(json!([1, 2])))
        );
        assert_eq!(
            serde_json::to_value(&suspend).unwrap()["state"],
            json!([1, 2])
        );
        assert!(
            decode(json!({"result": {}, "state": 1})).is_err(),
            "a terminal result carries no state for a next step"
        );
    }

    #[test]
    fn unknown_effects_key_is_still_refused() {
        assert!(decode(json!({ "effects": { "spawns": [] } })).is_err());
    }

    fn progress_json() -> serde_json::Value {
        json!({
            "title": "t", "state": "running", "phases": [], "active": [], "spawned": 0,
            "budget": { "total": 1, "spent": 0, "remaining": 1 },
            "last_event": "e", "observed_terminal_children": 0, "revision": 0
        })
    }

    #[test]
    fn progress_last_event_at_is_optional() {
        let absent: JobProgress = serde_json::from_value(progress_json()).expect("absent");
        assert_eq!(absent.last_event_at, None);
        let mut null = progress_json();
        null["last_event_at"] = serde_json::Value::Null;
        let null: JobProgress = serde_json::from_value(null).expect("null");
        assert_eq!(null.last_event_at, None);
        let mut stamped = progress_json();
        stamped["last_event_at"] = json!("2026-10-14T21:24:00Z");
        let stamped: JobProgress = serde_json::from_value(stamped).expect("stamped");
        assert_eq!(
            stamped.last_event_at.as_deref(),
            Some("2026-10-14T21:24:00Z")
        );
        assert_eq!(
            serde_json::to_value(&absent).expect("encode"),
            progress_json()
        );
    }

    #[test]
    fn job_read_error_display_names_the_next_call() {
        let message = || "m".to_owned();
        let cases = [
            (
                JobReadError::NotAJob,
                "not a plugin job; find the job's address with `gents job list`",
            ),
            (
                JobReadError::NotAuthorized,
                "this job belongs to another session or requester; read it from the session that started it",
            ),
            (
                JobReadError::NotYetObservable,
                "no job row is visible at this address yet; retry shortly, or check the address with `gents job list`",
            ),
            (
                JobReadError::Unavailable { message: message() },
                "job store unavailable: m",
            ),
            (
                JobReadError::Malformed { message: message() },
                "job row does not decode: m; retrying will not help",
            ),
        ];
        for (error, text) in cases {
            assert_eq!(error.to_string(), text);
        }
        let boxed: Box<dyn std::error::Error + Send + Sync> = JobReadError::NotAJob.into();
        assert_eq!(
            boxed.downcast_ref::<JobReadError>(),
            Some(&JobReadError::NotAJob)
        );
    }
}
