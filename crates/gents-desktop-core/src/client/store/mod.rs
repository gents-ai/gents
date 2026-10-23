mod control_plane_lookups;
mod indexing;
mod merge_helpers;
mod merges;
mod scope;
mod session_lookups;
mod turns;

pub(crate) use indexing::sort_request_rows;

pub use turns::{
    folded_requests, queued_behind_turn, request_answer_owner, session_display_request,
    session_turn_request,
};

use rapidhash::{HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;

use gents::document_config::{
    Agent, AgentContext, AgentTargetDocument, ChainKeyBindingDocument, CompactionConfig,
    DatastoreToolSurfaceDocument, EventSource, InferenceBackend, InferenceBackendObservation,
    InferenceExecution, InferenceProfile, InferenceSampling, Node, Schedule, ScheduleObservation,
    SkillDocument, Task, ToolServiceRegistry, Tools, Trigger, TriggerObservation,
};
use gents::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_protocol::client_protocol::ClientTurnState;
use gents_protocol::row::{
    AgentRequestRow, AgentToolCallRow, CompactionEntryRow, GoalRow, MailboxItemRow,
    NodeReadinessRow, NodeRuntimeRow,
};
use gents_protocol::session::AgentSession;
use serde::Serialize;

use self::indexing::{clean_string, indexes_to_refs};
use self::merge_helpers::*;
use self::scope::nonblank;
pub(crate) use self::scope::StoreScope;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientStoreRows {
    pub nodes: Vec<Node>,
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
    #[serde(skip)]
    pub session_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub tool_call_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub compaction_entry_source_node_dids: Vec<Option<String>>,
    pub tasks: Vec<Task>,
    pub schedules: Vec<Schedule>,
    pub schedule_observations: Vec<ScheduleObservation>,
    pub triggers: Vec<Trigger>,
    pub trigger_observations: Vec<TriggerObservation>,
    #[serde(skip)]
    pub task_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub schedule_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub schedule_observation_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub trigger_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub trigger_observation_source_node_dids: Vec<Option<String>>,
    pub skills: Vec<SkillDocument>,
    #[serde(skip)]
    pub skill_source_node_dids: Vec<Option<String>>,
    pub tools: Vec<Tools>,
    #[serde(skip)]
    pub tools_source_node_dids: Vec<Option<String>>,
    pub contexts: Vec<AgentContext>,
    #[serde(skip)]
    pub context_source_node_dids: Vec<Option<String>>,
    pub compactions: Vec<CompactionConfig>,
    #[serde(skip)]
    pub compaction_source_node_dids: Vec<Option<String>>,
    pub inference_backends: Vec<InferenceBackend>,
    pub backend_observations: Vec<InferenceBackendObservation>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    #[serde(skip)]
    pub inference_sampling_source_node_dids: Vec<Option<String>>,
    pub inference_execution: Vec<InferenceExecution>,
    #[serde(skip)]
    pub inference_execution_source_node_dids: Vec<Option<String>>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub event_sources: Vec<EventSource>,
    #[serde(skip)]
    pub event_source_source_node_dids: Vec<Option<String>>,
    pub agent_targets: Vec<AgentTargetDocument>,
    #[serde(skip)]
    pub agent_target_source_node_dids: Vec<Option<String>>,
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    #[serde(skip)]
    pub datastore_tool_surface_source_node_dids: Vec<Option<String>>,
    pub chain_key_bindings: Vec<ChainKeyBindingDocument>,
    #[serde(skip)]
    pub chain_key_binding_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub operator_config_authoritative_node_dids: RapidHashSet<String>,
    #[serde(skip)]
    pub inference_backend_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub backend_observation_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub inference_profile_source_node_dids: Vec<Option<String>>,
    #[serde(skip)]
    pub tool_service_registry_source_node_dids: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct ClientStore {
    pub nodes: Vec<Node>,
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
    pub session_source_node_dids: Vec<Option<String>>,
    pub tool_call_source_node_dids: Vec<Option<String>>,
    pub compaction_entry_source_node_dids: Vec<Option<String>>,
    pub tasks: Vec<Task>,
    pub schedules: Vec<Schedule>,
    pub schedule_observations: Vec<ScheduleObservation>,
    pub triggers: Vec<Trigger>,
    pub trigger_observations: Vec<TriggerObservation>,
    pub task_source_node_dids: Vec<Option<String>>,
    pub schedule_source_node_dids: Vec<Option<String>>,
    pub schedule_observation_source_node_dids: Vec<Option<String>>,
    pub trigger_source_node_dids: Vec<Option<String>>,
    pub trigger_observation_source_node_dids: Vec<Option<String>>,
    pub skills: Vec<SkillDocument>,
    pub skill_source_node_dids: Vec<Option<String>>,
    pub tools: Vec<Tools>,
    pub tools_source_node_dids: Vec<Option<String>>,
    pub contexts: Vec<AgentContext>,
    pub context_source_node_dids: Vec<Option<String>>,
    pub compactions: Vec<CompactionConfig>,
    pub compaction_source_node_dids: Vec<Option<String>>,
    pub inference_backends: Vec<InferenceBackend>,
    pub backend_observations: Vec<InferenceBackendObservation>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    pub inference_sampling_source_node_dids: Vec<Option<String>>,
    pub inference_execution: Vec<InferenceExecution>,
    pub inference_execution_source_node_dids: Vec<Option<String>>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub event_sources: Vec<EventSource>,
    pub event_source_source_node_dids: Vec<Option<String>>,
    pub agent_targets: Vec<AgentTargetDocument>,
    pub agent_target_source_node_dids: Vec<Option<String>>,
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    pub datastore_tool_surface_source_node_dids: Vec<Option<String>>,
    pub chain_key_bindings: Vec<ChainKeyBindingDocument>,
    pub chain_key_binding_source_node_dids: Vec<Option<String>>,
    pub operator_config_authoritative_node_dids: RapidHashSet<String>,
    pub inference_backend_source_node_dids: Vec<Option<String>>,
    pub backend_observation_source_node_dids: Vec<Option<String>>,
    pub inference_profile_source_node_dids: Vec<Option<String>>,
    pub tool_service_registry_source_node_dids: Vec<Option<String>>,
    transcript_messages_by_session_id: RapidHashMap<String, Vec<usize>>,
    output_segments_by_request_doc_id: RapidHashMap<String, Vec<usize>>,
    requests_by_session_id: RapidHashMap<String, Vec<usize>>,
    tool_calls_by_session_id: RapidHashMap<String, Vec<usize>>,
    runtimes_by_node_did: RapidHashMap<String, usize>,
    node_readiness_by_node_did: RapidHashMap<String, usize>,
    request_index_by_id: RapidHashMap<String, usize>,
    request_index_by_doc_id: RapidHashMap<String, Option<usize>>,
    unscoped_placeholder_request_ids: RapidHashSet<String>,
    mailbox_index_by_doc_id: RapidHashMap<String, Option<usize>>,
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
    pub(crate) fn merge_scope(&self, incoming: &ClientStore) -> StoreScope {
        let mut scope = StoreScope::default();
        scope.extend_rows(incoming);

        for row in &incoming.requests {
            if row.node_did.is_some()
                && self
                    .unscoped_placeholder_request_ids
                    .contains(&row.request_id)
            {
                return StoreScope::All;
            }
            let Some(doc_id) = row.doc_id.as_deref() else {
                continue;
            };
            match self.request_index_by_doc_id.get(doc_id) {
                Some(Some(index)) => {
                    let Some(previous) = self.requests.get(*index) else {
                        return StoreScope::All;
                    };
                    if previous.doc_id.as_deref() != Some(doc_id) {
                        return StoreScope::All;
                    }
                    let Some(owner) = previous.node_did.as_deref() else {
                        return StoreScope::All;
                    };
                    scope.insert_node(owner);
                }
                Some(None) => return StoreScope::All,
                None => {}
            }
        }

        for mailbox in &incoming.mailbox_items {
            let Some(doc_id) = nonblank(Some(&mailbox.doc_id)) else {
                return StoreScope::All;
            };
            match self.mailbox_index_by_doc_id.get(doc_id) {
                Some(Some(index)) => {
                    let Some(previous) = self.mailbox_items.get(*index) else {
                        return StoreScope::All;
                    };
                    if nonblank(Some(&previous.doc_id)) != Some(doc_id) {
                        return StoreScope::All;
                    }
                    scope.insert_node(&previous.node_did);
                }
                Some(None) => return StoreScope::All,
                None => {}
            }
        }

        scope
    }

    pub(crate) fn replacement_scope(&self, node_did: &str, incoming: &ClientStore) -> StoreScope {
        let Some(node_did) = nonblank(Some(node_did)) else {
            return StoreScope::All;
        };
        let mut scope = self.merge_scope(incoming);
        scope.insert_node(node_did);
        scope
    }

    pub(crate) fn request_event_scope(&self, doc_id: Option<&str>) -> StoreScope {
        let Some(doc_id) = nonblank(doc_id) else {
            return StoreScope::All;
        };
        match self.request_index_by_doc_id.get(doc_id) {
            Some(Some(index)) => {
                let Some(row) = self.requests.get(*index) else {
                    return StoreScope::All;
                };
                if nonblank(row.doc_id.as_deref()) != Some(doc_id) {
                    return StoreScope::All;
                }
                match nonblank(row.node_did.as_deref()) {
                    Some(node_did) => StoreScope::node(node_did.to_owned()),
                    None => StoreScope::All,
                }
            }
            Some(None) | None => StoreScope::All,
        }
    }

    pub(crate) fn into_observer_projection(mut self) -> Self {
        self.transcript_messages.clear();
        self.output_segments.clear();
        self.transcript_messages_by_session_id.clear();
        self.output_segments_by_request_doc_id.clear();
        self.tool_calls.clear();
        self.tool_call_source_node_dids.clear();
        self.tool_calls_by_session_id.clear();
        self.compaction_entries.clear();
        self.compaction_entry_source_node_dids.clear();
        self
    }
}

pub fn source_matches_node(
    sources: &[Option<String>],
    row_index: usize,
    node_did: &str,
    require_source_scope: bool,
) -> bool {
    match sources.get(row_index).and_then(|source| source.as_deref()) {
        Some(source_node_did) => source_node_did == node_did,
        None => !require_source_scope,
    }
}

#[cfg(test)]
mod tests {
    mod observer_projection;
    mod store_semantics;
}
