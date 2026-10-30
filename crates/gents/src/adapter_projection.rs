use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::run_timeline::{RunTimeline, RunTimelineEvent, TimelineRenderedRequestEvent};

mod atif;
mod build;
mod eval;
mod jsonl;
mod langgraph;
mod redaction;
mod schema;
mod validate;

pub use atif::{
    root_observed_model, AtifAgent, AtifFinalMetrics, AtifObservation, AtifObservationResult,
    AtifStep, AtifStepSource, AtifToolCall, AtifTrajectory, ATIF_SCHEMA_VERSION,
};
pub use build::{build_adapter_projection, build_external_adapter_projection};
pub use eval::adapter_projection_eval_jsonl_records;
pub use jsonl::{adapter_projection_jsonl_records, adapter_projection_native_json};
pub use schema::{
    adapter_projection_eval_jsonl_record_schema, adapter_projection_json_schema,
    adapter_projection_jsonl_record_schema, adapter_projection_native_json_schema,
    adapter_projection_schema_index,
};
pub use validate::validate_adapter_projection_contract;

pub const ADAPTER_PROJECTION_VERSION: &str = "v1";
pub const RUN_TIMELINE_PROJECTION_ID: &str = "run_timeline";
pub const EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID: &str = "external_adapter_capture";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterProjectionKind {
    AtifTrajectory,
    #[serde(rename = "openai_codex_run_trace")]
    OpenAiCodexRunTrace,
    #[serde(rename = "langgraph_state_history")]
    LangGraphStateHistory,
    MultiAgentTask,
}

impl AdapterProjectionKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::AtifTrajectory => "atif_trajectory",
            Self::OpenAiCodexRunTrace => "openai_codex_run_trace",
            Self::LangGraphStateHistory => "langgraph_state_history",
            Self::MultiAgentTask => "multi_agent_task",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::AtifTrajectory => "Agent Trajectory Interchange Format (ATIF) v1.7",
            Self::OpenAiCodexRunTrace => "OpenAI/Codex Run Trace Projection",
            Self::LangGraphStateHistory => "LangGraph State History Projection",
            Self::MultiAgentTask => "Multi-Agent Task Projection",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionRedactionMode {
    Full,
    TrainingSafe,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionContext {
    pub actor_did: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
}

impl Default for ProjectionContext {
    fn default() -> Self {
        Self {
            actor_did: None,
            redaction_mode: ProjectionRedactionMode::Full,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterProjectionEnvelope {
    pub projection_id: String,
    pub projection_version: String,
    pub source_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_request_doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
    pub provenance: ProjectionProvenance,
    /// Never bodies: the timeline never selects `request_json`, so a captured
    /// body cannot reach this envelope in any redaction mode; the one body read
    /// in the system is the CLI's explicit `--include-body`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rendered_captures: Vec<TimelineRenderedRequestEvent>,
    pub output: AdapterProjection,
}

fn rendered_capture_events(timeline: &RunTimeline) -> Vec<TimelineRenderedRequestEvent> {
    timeline
        .events
        .iter()
        .filter_map(|event| match event {
            RunTimelineEvent::RenderedRequest(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn rendered_captures_json(timeline: &RunTimeline) -> Option<Value> {
    let captures = rendered_capture_events(timeline);
    if captures.is_empty() {
        return None;
    }
    serde_json::to_value(captures).ok()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionProvenance {
    pub runtime: String,
    pub source_projection_id: String,
    pub source_projection_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_did: Option<String>,
    /// Explicitly records whether the projection came from current durable
    /// state. Missing provenance must not be interpreted as a known status.
    pub source_version_status: ProjectionSourceVersionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rendered_request_refs: Vec<RenderedRequestProvenanceRef>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionSourceVersionStatus {
    /// The projection was built from the latest visible DefraDB documents.
    /// Their exact composite versions are not pinned in this envelope.
    #[default]
    CurrentStateCapturedOnly,
    /// A derived view of an external framework capture.  It is explicitly not
    /// a claim about canonical DefraDB documents, headers, or execution state.
    ExternalAdapterCapture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedRequestProvenanceRef {
    pub capture_doc_id: String,
    pub request_doc_id: String,
    pub request_commit_cid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "adapter", content = "projection", rename_all = "snake_case")]
pub enum AdapterProjection {
    AtifTrajectory(AtifTrajectory),
    #[serde(rename = "openai_codex_run_trace")]
    OpenAiCodexRunTrace(OpenAiCodexRunTraceProjection),
    #[serde(rename = "langgraph_state_history")]
    LangGraphStateHistory(LangGraphStateHistoryProjection),
    MultiAgentTask(MultiAgentTaskProjection),
}

impl AdapterProjection {
    pub fn kind(&self) -> AdapterProjectionKind {
        match self {
            Self::AtifTrajectory(_) => AdapterProjectionKind::AtifTrajectory,
            Self::OpenAiCodexRunTrace(_) => AdapterProjectionKind::OpenAiCodexRunTrace,
            Self::LangGraphStateHistory(_) => AdapterProjectionKind::LangGraphStateHistory,
            Self::MultiAgentTask(_) => AdapterProjectionKind::MultiAgentTask,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterProjectionJsonlRecord {
    pub projection_id: String,
    pub projection_version: String,
    pub source_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
    pub record_kind: String,
    pub record_index: usize,
    pub record_id: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterProjectionEvalJsonlRecord {
    pub projection_id: String,
    pub projection_version: String,
    pub source_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
    pub record_index: usize,
    pub record_id: String,
    pub sample_kind: String,
    pub adapter_record_kind: String,
    pub adapter_record_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterProjectionContractError {
    pub violations: Vec<String>,
}

impl fmt::Display for AdapterProjectionContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "adapter projection contract failed: {}",
            self.violations.join("; ")
        )
    }
}

impl std::error::Error for AdapterProjectionContractError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiCodexRunTraceProjection {
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub items: Vec<OpenAiCodexTraceItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub child_run_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiCodexTraceItem {
    Request {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        node_did: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_request_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    Message {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        role: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    ToolCall {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        name: String,
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        child_run_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_at: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LangGraphStateHistoryProjection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub checkpoint_id: String,
    pub root_request_id: String,
    pub values: BTreeMap<String, Value>,
    pub nodes: Vec<LangGraphNode>,
    pub edges: Vec<LangGraphEdge>,
    pub tasks: Vec<LangGraphTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangGraphNode {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangGraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangGraphTask {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiAgentTaskProjection {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub participants: Vec<MultiAgentParticipant>,
    pub messages: Vec<MultiAgentMessage>,
    pub delegations: Vec<MultiAgentDelegation>,
    pub tool_events: Vec<MultiAgentToolEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentParticipant {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentMessage {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentDelegation {
    pub parent_request_id: String,
    pub child_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentToolEvent {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub tool_name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_service_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
}

#[cfg(test)]
mod tests;
