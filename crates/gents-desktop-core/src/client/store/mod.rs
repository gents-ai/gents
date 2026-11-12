mod control_plane_lookups;
mod indexing;
mod merge_helpers;
mod merges;
mod session_lookups;
mod session_summaries;
mod turns;

pub(crate) use indexing::sort_request_rows;

pub use session_summaries::{session_summary_entries, SessionSummaryEntry};
pub use turns::{
    folded_requests, queued_behind_turn, request_answer_owner, session_display_request,
    session_turn_request,
};

use rapidhash::{RapidHashMap, RapidHashSet};
use std::sync::Arc;

use gents_documents::{
    Agent, AgentContext, AgentTargetDocument, CompactionConfig, DatastoreToolSurfaceDocument,
    DefaultAgentSelection, EventSource, InferenceBackend, InferenceBackendObservation,
    InferenceExecution, InferenceProfile, InferenceSampling, Node, Schedule, ScheduleObservation,
    SkillDocument, Task, ToolServiceRegistry, Tools, Trigger, TriggerObservation,
};
use gents_protocol::client_protocol::ClientTurnState;
use gents_protocol::output::rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_protocol::row::{
    AgentRequestRow, AgentToolCallRow, CompactionEntryRow, GoalRow, MailboxItemRow,
    NodeReadinessRow, NodeRuntimeRow,
};
use gents_protocol::session::AgentSession;
use serde::Serialize;

use self::indexing::{clean_string, indexes_to_refs};
use self::merge_helpers::*;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientStoreRows {
    pub nodes: Vec<Node>,
    pub default_agent_selections: Vec<DefaultAgentSelection>,
    pub agents: Vec<Agent>,
    pub runtimes: Vec<NodeRuntimeRow>,
    pub node_readiness: Vec<NodeReadinessRow>,
    pub requests: Vec<AgentRequestRow>,
    pub mailbox_items: Vec<MailboxItemRow>,
    #[serde(skip)]
    pub transcript_messages: Vec<TranscriptMessageRow>,
    #[serde(skip)]
    pub output_segments: Vec<OutputSegmentRow>,
    pub sessions: Vec<AgentSession>,
    pub goals: Vec<GoalRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub tasks: Vec<Task>,
    pub schedules: Vec<Schedule>,
    pub schedule_observations: Vec<ScheduleObservation>,
    pub triggers: Vec<Trigger>,
    pub trigger_observations: Vec<TriggerObservation>,
    pub skills: Vec<SkillDocument>,
    pub tools: Vec<Tools>,
    pub contexts: Vec<AgentContext>,
    pub compactions: Vec<CompactionConfig>,
    pub inference_backends: Vec<InferenceBackend>,
    pub backend_observations: Vec<InferenceBackendObservation>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    pub inference_execution: Vec<InferenceExecution>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub event_sources: Vec<EventSource>,
    pub agent_targets: Vec<AgentTargetDocument>,
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    #[serde(skip)]
    pub operator_config_authoritative_node_dids: RapidHashSet<String>,
}

#[derive(Debug, Clone)]
pub struct ClientStore {
    pub nodes: Vec<Node>,
    pub default_agent_selections: Vec<DefaultAgentSelection>,
    pub agents: Vec<Agent>,
    pub runtimes: Vec<NodeRuntimeRow>,
    pub node_readiness: Vec<NodeReadinessRow>,
    pub requests: Vec<AgentRequestRow>,
    pub mailbox_items: Vec<MailboxItemRow>,
    pub transcript_messages: Vec<TranscriptMessageRow>,
    pub output_segments: Vec<OutputSegmentRow>,
    pub sessions: Vec<AgentSession>,
    pub goals: Vec<GoalRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub tasks: Vec<Task>,
    pub schedules: Vec<Schedule>,
    pub schedule_observations: Vec<ScheduleObservation>,
    pub triggers: Vec<Trigger>,
    pub trigger_observations: Vec<TriggerObservation>,
    pub skills: Vec<SkillDocument>,
    pub tools: Vec<Tools>,
    pub contexts: Vec<AgentContext>,
    pub compactions: Vec<CompactionConfig>,
    pub inference_backends: Vec<InferenceBackend>,
    pub backend_observations: Vec<InferenceBackendObservation>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    pub inference_execution: Vec<InferenceExecution>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub event_sources: Vec<EventSource>,
    pub agent_targets: Vec<AgentTargetDocument>,
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    pub operator_config_authoritative_node_dids: RapidHashSet<String>,
    transcript_messages_by_session_id: RapidHashMap<String, Vec<usize>>,
    output_segments_by_request_doc_id: RapidHashMap<String, Vec<usize>>,
    requests_by_session_id: RapidHashMap<String, Vec<usize>>,
    tool_calls_by_session_id: RapidHashMap<String, Vec<usize>>,
    runtimes_by_node_did: RapidHashMap<String, usize>,
    node_readiness_by_node_did: RapidHashMap<String, usize>,
    request_index_by_id: RapidHashMap<String, usize>,
    request_index_by_doc_id: RapidHashMap<String, usize>,
}

#[derive(Debug)]
pub struct TranscriptView<'a> {
    pub messages: Vec<&'a TranscriptMessageRow>,
    pub output_segments: Vec<&'a OutputSegmentRow>,
    pub tool_calls: Vec<&'a AgentToolCallRow>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TaskRecentRuns {
    pub total_fires: u64,
    pub last_attempt_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub schedule_count: usize,
    pub event_count: usize,
}

impl Default for ClientStore {
    fn default() -> Self {
        Self::from_rows(ClientStoreRows::default())
    }
}

pub type SharedClientStore = Arc<ClientStore>;

impl ClientStore {
    pub(crate) fn into_observer_projection(mut self) -> Self {
        self.transcript_messages.clear();
        self.output_segments.clear();
        self.transcript_messages_by_session_id.clear();
        self.output_segments_by_request_doc_id.clear();
        self.tool_calls.clear();
        self.tool_calls_by_session_id.clear();
        self.compaction_entries.clear();
        self
    }
}

#[cfg(test)]
mod tests {
    mod observer_projection;
    mod session_summaries;
    mod store_semantics;
}
