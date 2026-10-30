use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{
    JobCaller, JobCeilingView, JobChildView, JobControlEcho, JobGuestPin, JobOrigin,
    JobPauseReason, JobPin, JobProgress, JobRef, JobResume, JobRootCall, JobRowView,
};

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
    /// The model's script text, sent on every step; its presence makes the
    /// run `dynamic`, and a plugin that declares the `run_script` host import
    /// asks the host to evaluate it.
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
