mod control_plane_lookups;
mod indexing;
mod merge_helpers;
mod merges;
mod observer_projection;
mod session_lookups;
mod turns;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gents_protocol::client_protocol::ClientTurnState;
use gents_protocol::row::{
    AgentBehaviorReadinessRow, AgentBehaviorRow, AgentConversationRow, AgentMessageRow,
    AgentPrincipalRow, AgentRequestRow, AgentResponseRow, AgentRuntimeRow, AgentSessionRow,
    AgentToolCallRow, AgentToolResultRow, CompactionEntryRow, EventTriggerRow, GoalRow,
    InferenceBackendRow, InferenceProfileRow, MailboxItemRow, ScheduleRow, SkillRow, TaskRow,
    ToolSelectionRow, ToolServiceRegistryRow,
};
use serde::Serialize;

use self::indexing::{clean_string, indexes_to_refs};
use self::merge_helpers::*;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientStoreRows {
    pub agent_principals: Vec<AgentPrincipalRow>,
    pub behaviors: Vec<AgentBehaviorRow>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub behavior_readiness: Vec<AgentBehaviorReadinessRow>,
    pub conversations: Vec<AgentConversationRow>,
    pub requests: Vec<AgentRequestRow>,
    pub mailbox_items: Vec<MailboxItemRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSessionRow>,
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
    pub tasks: Vec<TaskRow>,
    pub schedules: Vec<ScheduleRow>,
    pub event_triggers: Vec<EventTriggerRow>,
    #[serde(skip)]
    pub task_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub schedule_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub event_trigger_source_agent_dids: Vec<Option<String>>,
    pub skills: Vec<SkillRow>,
    #[serde(skip)]
    pub skill_source_agent_dids: Vec<Option<String>>,
    pub tool_selections: Vec<ToolSelectionRow>,
    pub inference_backends: Vec<InferenceBackendRow>,
    pub inference_profiles: Vec<InferenceProfileRow>,
    pub tool_service_registries: Vec<ToolServiceRegistryRow>,
    #[serde(skip)]
    pub inference_backend_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub inference_profile_source_agent_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub tool_service_registry_source_agent_dids: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct ClientStore {
    pub agent_principals: Vec<AgentPrincipalRow>,
    pub behaviors: Vec<AgentBehaviorRow>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub behavior_readiness: Vec<AgentBehaviorReadinessRow>,
    pub conversations: Vec<AgentConversationRow>,
    pub requests: Vec<AgentRequestRow>,
    pub mailbox_items: Vec<MailboxItemRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSessionRow>,
    pub goals: Vec<GoalRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub tool_results: Vec<AgentToolResultRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub message_source_agent_dids: Vec<Option<String>>,
    pub session_source_agent_dids: Vec<Option<String>>,
    pub tool_call_source_agent_dids: Vec<Option<String>>,
    pub tool_result_source_agent_dids: Vec<Option<String>>,
    pub compaction_entry_source_agent_dids: Vec<Option<String>>,
    pub tasks: Vec<TaskRow>,
    pub schedules: Vec<ScheduleRow>,
    pub event_triggers: Vec<EventTriggerRow>,
    pub task_source_agent_dids: Vec<Option<String>>,
    pub schedule_source_agent_dids: Vec<Option<String>>,
    pub event_trigger_source_agent_dids: Vec<Option<String>>,
    pub skills: Vec<SkillRow>,
    pub skill_source_agent_dids: Vec<Option<String>>,
    pub tool_selections: Vec<ToolSelectionRow>,
    pub inference_backends: Vec<InferenceBackendRow>,
    pub inference_profiles: Vec<InferenceProfileRow>,
    pub tool_service_registries: Vec<ToolServiceRegistryRow>,
    pub inference_backend_source_agent_dids: Vec<Option<String>>,
    pub inference_profile_source_agent_dids: Vec<Option<String>>,
    pub tool_service_registry_source_agent_dids: Vec<Option<String>>,
    conversations_by_agent_did: HashMap<String, Vec<usize>>,
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
/// triggers (Schedule + EventTrigger) that reference it.
///
/// The apply path owns the `Task` description while the trigger engine
/// owns per-trigger fire bookkeeping on `Schedule` and `EventTrigger`.
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
    pub event_trigger_count: usize,
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
