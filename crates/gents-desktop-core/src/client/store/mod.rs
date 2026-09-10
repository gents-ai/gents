mod control_plane_lookups;
mod indexing;
mod merge_helpers;
mod merges;
mod observer_projection;
mod session_lookups;
mod turns;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gents::document_config::{
    AgentBehavior, AgentContext, AgentPrincipal, ChainKeyBindingDocument, CompactionConfig,
    DatastoreToolSurfaceDocument, EventSource, InferenceBackend, InferenceBackendObservation,
    InferenceExecution, InferenceProfile, InferenceSampling, Schedule, ScheduleObservation,
    SkillDocument, SubagentTargetDocument, Task, ToolServiceRegistry, Tools, Trigger,
    TriggerObservation,
};
use gents_protocol::client_protocol::ClientTurnState;
use gents_protocol::row::{
    AgentBehaviorReadinessRow, AgentMessageRow, AgentRequestRow, AgentResponseRow, AgentRuntimeRow,
    AgentToolCallRow, AgentToolResultRow, CompactionEntryRow, GoalRow, MailboxItemRow,
};
use gents_protocol::session::AgentSession;
use serde::Serialize;

use self::indexing::{clean_string, indexes_to_refs};
use self::merge_helpers::*;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientStoreRows {
    pub agent_principals: Vec<AgentPrincipal>,
    pub behaviors: Vec<AgentBehavior>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub behavior_readiness: Vec<AgentBehaviorReadinessRow>,
    pub requests: Vec<AgentRequestRow>,
    pub mailbox_items: Vec<MailboxItemRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSession>,
    pub goals: Vec<GoalRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub tool_results: Vec<AgentToolResultRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    #[serde(skip)]
    pub message_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub session_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub tool_call_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub tool_result_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub compaction_entry_source_agent_dids: Vec<Option<String>>,
    pub tasks: Vec<Task>,
    pub schedules: Vec<Schedule>,
    pub schedule_observations: Vec<ScheduleObservation>,
    pub triggers: Vec<Trigger>,
    pub trigger_observations: Vec<TriggerObservation>,
    #[serde(skip)]
    pub task_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub schedule_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub schedule_observation_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub trigger_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub trigger_observation_source_agent_dids: Vec<Option<String>>,
    pub skills: Vec<SkillDocument>,
    #[serde(skip)]
    pub skill_source_agent_dids: Vec<Option<String>>,
    pub tools: Vec<Tools>,
    #[serde(skip)]
    pub tools_source_agent_dids: Vec<Option<String>>,
    pub contexts: Vec<AgentContext>,
    #[serde(skip)]
    pub context_source_agent_dids: Vec<Option<String>>,
    pub compactions: Vec<CompactionConfig>,
    #[serde(skip)]
    pub compaction_source_agent_dids: Vec<Option<String>>,
    pub inference_backends: Vec<InferenceBackend>,
    pub backend_observations: Vec<InferenceBackendObservation>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    #[serde(skip)]
    pub inference_sampling_source_agent_dids: Vec<Option<String>>,
    pub inference_execution: Vec<InferenceExecution>,
    #[serde(skip)]
    pub inference_execution_source_agent_dids: Vec<Option<String>>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub event_sources: Vec<EventSource>,
    #[serde(skip)]
    pub event_source_source_agent_dids: Vec<Option<String>>,
    pub subagent_targets: Vec<SubagentTargetDocument>,
    #[serde(skip)]
    pub subagent_target_source_agent_dids: Vec<Option<String>>,
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    #[serde(skip)]
    pub datastore_tool_surface_source_agent_dids: Vec<Option<String>>,
    pub chain_key_bindings: Vec<ChainKeyBindingDocument>,
    #[serde(skip)]
    pub chain_key_binding_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub inference_backend_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub backend_observation_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub inference_profile_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub tool_service_registry_source_agent_dids: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct ClientStore {
    pub agent_principals: Vec<AgentPrincipal>,
    pub behaviors: Vec<AgentBehavior>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub behavior_readiness: Vec<AgentBehaviorReadinessRow>,
    pub requests: Vec<AgentRequestRow>,
    pub mailbox_items: Vec<MailboxItemRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSession>,
    pub goals: Vec<GoalRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub tool_results: Vec<AgentToolResultRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub message_source_agent_dids: Vec<Option<String>>,
    pub session_source_agent_dids: Vec<Option<String>>,
    pub tool_call_source_agent_dids: Vec<Option<String>>,
    pub tool_result_source_agent_dids: Vec<Option<String>>,
    pub compaction_entry_source_agent_dids: Vec<Option<String>>,
    pub tasks: Vec<Task>,
    pub schedules: Vec<Schedule>,
    pub schedule_observations: Vec<ScheduleObservation>,
    pub triggers: Vec<Trigger>,
    pub trigger_observations: Vec<TriggerObservation>,
    pub task_source_agent_dids: Vec<Option<String>>,
    pub schedule_source_agent_dids: Vec<Option<String>>,
    pub schedule_observation_source_agent_dids: Vec<Option<String>>,
    pub trigger_source_agent_dids: Vec<Option<String>>,
    pub trigger_observation_source_agent_dids: Vec<Option<String>>,
    pub skills: Vec<SkillDocument>,
    pub skill_source_agent_dids: Vec<Option<String>>,
    pub tools: Vec<Tools>,
    pub tools_source_agent_dids: Vec<Option<String>>,
    pub contexts: Vec<AgentContext>,
    pub context_source_agent_dids: Vec<Option<String>>,
    pub compactions: Vec<CompactionConfig>,
    pub compaction_source_agent_dids: Vec<Option<String>>,
    pub inference_backends: Vec<InferenceBackend>,
    pub backend_observations: Vec<InferenceBackendObservation>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    pub inference_sampling_source_agent_dids: Vec<Option<String>>,
    pub inference_execution: Vec<InferenceExecution>,
    pub inference_execution_source_agent_dids: Vec<Option<String>>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub event_sources: Vec<EventSource>,
    pub event_source_source_agent_dids: Vec<Option<String>>,
    pub subagent_targets: Vec<SubagentTargetDocument>,
    pub subagent_target_source_agent_dids: Vec<Option<String>>,
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    pub datastore_tool_surface_source_agent_dids: Vec<Option<String>>,
    pub chain_key_bindings: Vec<ChainKeyBindingDocument>,
    pub chain_key_binding_source_agent_dids: Vec<Option<String>>,
    pub inference_backend_source_agent_dids: Vec<Option<String>>,
    pub backend_observation_source_agent_dids: Vec<Option<String>>,
    pub inference_profile_source_agent_dids: Vec<Option<String>>,
    pub tool_service_registry_source_agent_dids: Vec<Option<String>>,
    messages_by_session_id: HashMap<String, Vec<usize>>,
    requests_by_session_id: HashMap<String, Vec<usize>>,
    tool_calls_by_session_id: HashMap<String, Vec<usize>>,
    tool_results_by_session_id: HashMap<String, Vec<usize>>,
    runtimes_by_agent_did: HashMap<String, usize>,
    behavior_readiness_by_agent_did: HashMap<String, usize>,
    latest_response_by_request_id: HashMap<String, usize>,
    response_index_by_key: HashMap<String, usize>,
    request_index_by_id: HashMap<String, usize>,
}

#[derive(Debug)]
pub struct TranscriptView<'a> {
    pub messages: Vec<&'a AgentMessageRow>,
    pub tool_calls: Vec<&'a AgentToolCallRow>,
    pub tool_results: Vec<&'a AgentToolResultRow>,
}

/// Aggregated recent-run bookkeeping for a task, rolled up across all
/// canonical triggers that reference it.
///
/// The apply path owns the `Task` description while the trigger engine
/// owns per-trigger fire bookkeeping on `Trigger`.
/// Operators looking at a single task need to see "how often has this
/// task actually been fired, and what happened last time?" without
/// having to click into every trigger individually -- this struct rolls
/// those numbers up for the Task detail view.
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

#[cfg(test)]
mod tests {
    mod observer_projection;
    mod store_semantics;
}
