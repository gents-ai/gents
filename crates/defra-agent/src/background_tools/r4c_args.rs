//! Argument and envelope types for the R4c agent-facing background-work tools.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const DEFAULT_LIST_LIMIT: u32 = 20;
const MAX_LIST_LIMIT: u32 = 50;
const DEFAULT_TRANSCRIPT_LIMIT: u32 = 20;
const MAX_TRANSCRIPT_LIMIT: u32 = 100;
const DEFAULT_TRANSCRIPT_MAX_CHARS: u32 = 6000;
const MAX_TRANSCRIPT_MAX_CHARS: u32 = 24000;
const DEFAULT_READ_TOOL_OUTPUT_BYTES: u32 = 16384;
const MAX_READ_TOOL_OUTPUT_BYTES: u32 = 262144;

pub(crate) const PER_TOOL_RESULT_SNIPPET_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ListStatusFilter {
    #[default]
    Running,
    Terminal,
    All,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListSubagentsArgs {
    #[serde(default)]
    pub(crate) status: ListStatusFilter,
    #[serde(default = "default_list_limit")]
    pub(crate) limit: u32,
}

fn default_list_limit() -> u32 {
    DEFAULT_LIST_LIMIT
}

impl Default for ListSubagentsArgs {
    fn default() -> Self {
        Self {
            status: ListStatusFilter::default(),
            limit: DEFAULT_LIST_LIMIT,
        }
    }
}

impl ListSubagentsArgs {
    pub(crate) fn validated_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_LIST_LIMIT)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ListBackgroundToolsArgs {
    #[serde(default)]
    pub(crate) status: ListStatusFilter,
    #[serde(default = "default_list_limit")]
    pub(crate) limit: u32,
}

impl ListBackgroundToolsArgs {
    pub(crate) fn validated_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_LIST_LIMIT)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReadSubagentTranscriptArgs {
    pub(crate) child_request_id: String,
    #[serde(default)]
    pub(crate) since_sequence: u64,
    #[serde(default = "default_transcript_limit")]
    pub(crate) limit: u32,
    #[serde(default = "default_transcript_max_chars")]
    pub(crate) max_chars: u32,
    #[serde(default)]
    pub(crate) include_user_messages: bool,
    #[serde(default)]
    pub(crate) include_tool_results: bool,
}

fn default_transcript_limit() -> u32 {
    DEFAULT_TRANSCRIPT_LIMIT
}

fn default_transcript_max_chars() -> u32 {
    DEFAULT_TRANSCRIPT_MAX_CHARS
}

impl ReadSubagentTranscriptArgs {
    pub(crate) fn validated_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_TRANSCRIPT_LIMIT)
    }

    pub(crate) fn validated_max_chars(&self) -> u32 {
        self.max_chars.clamp(64, MAX_TRANSCRIPT_MAX_CHARS)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReadToolOutputArgs {
    pub(crate) tool_call_id: String,
    #[serde(default = "default_read_tool_output_bytes")]
    pub(crate) max_bytes_per_stream: u32,
}

fn default_read_tool_output_bytes() -> u32 {
    DEFAULT_READ_TOOL_OUTPUT_BYTES
}

impl ReadToolOutputArgs {
    pub(crate) fn validated_max_bytes(&self) -> u32 {
        self.max_bytes_per_stream
            .clamp(256, MAX_READ_TOOL_OUTPUT_BYTES)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SteerSubagentArgs {
    pub(crate) child_request_id: String,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) interrupt: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListSubagentsEntry {
    pub child_request_id: String,
    pub child_session_id: String,
    /// Friendly model-facing name of the subagent target (from the spawn args).
    /// Matches the `name` passed to `spawn_subagent`. Empty string if the
    /// bridge args did not carry a name (legacy or malformed record).
    pub name: String,
    pub behavior_id: String,
    pub deployment_id: String,
    pub await_mode: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_update: DateTime<Utc>,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListSubagentsResponse {
    pub read_at: DateTime<Utc>,
    pub truncated: bool,
    pub entries: Vec<ListSubagentsEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListBackgroundToolsEntry {
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    pub(crate) deployment_id: String,
    pub(crate) await_mode: String,
    pub(crate) status: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) last_update: DateTime<Utc>,
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListBackgroundToolsResponse {
    pub(crate) read_at: DateTime<Utc>,
    pub(crate) truncated: bool,
    pub(crate) entries: Vec<ListBackgroundToolsEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadSubagentTranscriptResponse {
    pub(crate) child_request_id: String,
    pub(crate) child_session_id: String,
    pub(crate) from_sequence: u64,
    pub(crate) through_sequence: u64,
    pub(crate) next_sequence: u64,
    pub(crate) truncated: bool,
    pub(crate) transcript: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadToolOutputStream {
    pub(crate) bytes: String,
    pub(crate) truncated: bool,
    pub(crate) total_bytes_seen: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadToolOutputResponse {
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    pub(crate) status: String,
    pub(crate) stdout: ReadToolOutputStream,
    pub(crate) stderr: ReadToolOutputStream,
    pub(crate) exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SteerSubagentResponse {
    pub(crate) child_request_id: String,
    pub(crate) child_session_id: String,
    pub(crate) queued_request_id: String,
    pub(crate) interrupted_active_request_id: Option<String>,
    pub(crate) drained_wake_up_request_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_subagents_args_round_trip() {
        let args: ListSubagentsArgs = serde_json::from_value(json!({
            "status": "running",
            "limit": 20
        }))
        .expect("parse");
        assert_eq!(args.status, ListStatusFilter::Running);
        assert_eq!(args.limit, 20);
        assert_eq!(args.validated_limit(), 20);
    }

    #[test]
    fn list_subagents_args_defaults_and_caps() {
        let args: ListSubagentsArgs = serde_json::from_value(json!({})).expect("parse");
        assert_eq!(args.status, ListStatusFilter::Running);
        assert_eq!(args.limit, DEFAULT_LIST_LIMIT);

        let capped: ListSubagentsArgs =
            serde_json::from_value(json!({ "status": "all", "limit": 500 })).expect("parse");
        assert_eq!(capped.status, ListStatusFilter::All);
        assert_eq!(capped.validated_limit(), MAX_LIST_LIMIT);

        let floored: ListSubagentsArgs =
            serde_json::from_value(json!({ "limit": 0 })).expect("parse");
        assert_eq!(floored.validated_limit(), 1);
    }

    #[test]
    fn list_background_tools_args_round_trip_and_defaults() {
        let defaults: ListBackgroundToolsArgs =
            serde_json::from_value(json!({})).expect("parse defaults");
        assert_eq!(defaults.status, ListStatusFilter::Running);
        assert_eq!(defaults.limit, DEFAULT_LIST_LIMIT);

        let explicit: ListBackgroundToolsArgs = serde_json::from_value(json!({
            "status": "terminal",
            "limit": 51
        }))
        .expect("parse explicit");
        assert_eq!(explicit.status, ListStatusFilter::Terminal);
        assert_eq!(explicit.validated_limit(), MAX_LIST_LIMIT);
    }

    #[test]
    fn read_subagent_transcript_args_round_trip_and_defaults() {
        let defaults: ReadSubagentTranscriptArgs = serde_json::from_value(json!({
            "child_request_id": "child-1"
        }))
        .expect("parse defaults");
        assert_eq!(defaults.child_request_id, "child-1");
        assert_eq!(defaults.since_sequence, 0);
        assert_eq!(defaults.limit, DEFAULT_TRANSCRIPT_LIMIT);
        assert_eq!(defaults.max_chars, DEFAULT_TRANSCRIPT_MAX_CHARS);
        assert!(!defaults.include_user_messages);
        assert!(!defaults.include_tool_results);

        let explicit: ReadSubagentTranscriptArgs = serde_json::from_value(json!({
            "child_request_id": "child-2",
            "since_sequence": 7,
            "limit": 200,
            "max_chars": 30000,
            "include_user_messages": true,
            "include_tool_results": true
        }))
        .expect("parse explicit");
        assert_eq!(explicit.child_request_id, "child-2");
        assert_eq!(explicit.since_sequence, 7);
        assert_eq!(explicit.validated_limit(), MAX_TRANSCRIPT_LIMIT);
        assert_eq!(explicit.validated_max_chars(), MAX_TRANSCRIPT_MAX_CHARS);
        assert!(explicit.include_user_messages);
        assert!(explicit.include_tool_results);
    }

    #[test]
    fn read_tool_output_args_round_trip_and_defaults() {
        let defaults: ReadToolOutputArgs = serde_json::from_value(json!({
            "tool_call_id": "tool-1"
        }))
        .expect("parse defaults");
        assert_eq!(defaults.tool_call_id, "tool-1");
        assert_eq!(
            defaults.max_bytes_per_stream,
            DEFAULT_READ_TOOL_OUTPUT_BYTES
        );

        let explicit: ReadToolOutputArgs = serde_json::from_value(json!({
            "tool_call_id": "tool-2",
            "max_bytes_per_stream": 1
        }))
        .expect("parse explicit");
        assert_eq!(explicit.tool_call_id, "tool-2");
        assert_eq!(explicit.validated_max_bytes(), 256);
    }

    #[test]
    fn steer_subagent_args_round_trip_and_defaults() {
        let defaults: SteerSubagentArgs = serde_json::from_value(json!({
            "child_request_id": "child-1",
            "message": "continue"
        }))
        .expect("parse defaults");
        assert_eq!(defaults.child_request_id, "child-1");
        assert_eq!(defaults.message, "continue");
        assert!(!defaults.interrupt);

        let explicit: SteerSubagentArgs = serde_json::from_value(json!({
            "child_request_id": "child-2",
            "message": "restart with this",
            "interrupt": true
        }))
        .expect("parse explicit");
        assert_eq!(explicit.child_request_id, "child-2");
        assert_eq!(explicit.message, "restart with this");
        assert!(explicit.interrupt);
    }
}
