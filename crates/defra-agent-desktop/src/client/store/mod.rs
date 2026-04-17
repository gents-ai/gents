mod indexing;
mod turns;

use std::collections::HashMap;
use std::sync::Arc;

use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentConversationRow, AgentMessageRow, AgentPrincipalRow, AgentRequestRow,
    AgentResponseRow, AgentRuntimeRow, AgentSessionRow, AgentToolCallRow, AgentToolResultRow,
    CompactionEntryRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow,
    ToolSelectionRow, ToolServiceRegistryRow,
};
use serde::Serialize;

use self::indexing::{clean_string, indexes_to_refs};

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientStoreRows {
    pub agent_principals: Vec<AgentPrincipalRow>,
    pub behaviors: Vec<AgentBehaviorRow>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub conversations: Vec<AgentConversationRow>,
    pub requests: Vec<AgentRequestRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSessionRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub tool_results: Vec<AgentToolResultRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub scheduled_tasks: Vec<ScheduledTaskRow>,
    pub tool_selections: Vec<ToolSelectionRow>,
    pub inference_backends: Vec<InferenceBackendRow>,
    pub inference_profiles: Vec<InferenceProfileRow>,
    pub tool_service_registries: Vec<ToolServiceRegistryRow>,
}

#[derive(Debug, Clone)]
pub struct ClientStore {
    pub agent_principals: Vec<AgentPrincipalRow>,
    pub behaviors: Vec<AgentBehaviorRow>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub conversations: Vec<AgentConversationRow>,
    pub requests: Vec<AgentRequestRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSessionRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub tool_results: Vec<AgentToolResultRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub scheduled_tasks: Vec<ScheduledTaskRow>,
    pub tool_selections: Vec<ToolSelectionRow>,
    pub inference_backends: Vec<InferenceBackendRow>,
    pub inference_profiles: Vec<InferenceProfileRow>,
    pub tool_service_registries: Vec<ToolServiceRegistryRow>,
    conversations_by_agent_did: HashMap<String, Vec<usize>>,
    messages_by_session_id: HashMap<String, Vec<usize>>,
    requests_by_session_id: HashMap<String, Vec<usize>>,
    tool_calls_by_session_id: HashMap<String, Vec<usize>>,
    tool_results_by_session_id: HashMap<String, Vec<usize>>,
    runtimes_by_agent_did: HashMap<String, usize>,
    latest_response_by_request_id: HashMap<String, usize>,
    request_index_by_id: HashMap<String, usize>,
}

#[derive(Debug)]
pub struct TranscriptView<'a> {
    pub messages: Vec<&'a AgentMessageRow>,
    pub tool_calls: Vec<&'a AgentToolCallRow>,
    pub tool_results: Vec<&'a AgentToolResultRow>,
}

impl Default for ClientStore {
    fn default() -> Self {
        Self::from_rows(ClientStoreRows::default())
    }
}

impl ClientStore {
    pub fn conversation_rows(&self, agent_did: &str) -> Vec<&AgentConversationRow> {
        self.conversations_by_agent_did
            .get(agent_did)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .map(|index| &self.conversations[*index])
            .collect()
    }

    pub fn transcript(&self, session_id: &str) -> TranscriptView<'_> {
        TranscriptView {
            messages: indexes_to_refs(&self.messages, self.messages_by_session_id.get(session_id)),
            tool_calls: indexes_to_refs(
                &self.tool_calls,
                self.tool_calls_by_session_id.get(session_id),
            ),
            tool_results: indexes_to_refs(
                &self.tool_results,
                self.tool_results_by_session_id.get(session_id),
            ),
        }
    }

    pub fn requests_for_session(&self, session_id: &str) -> Vec<&AgentRequestRow> {
        indexes_to_refs(&self.requests, self.requests_by_session_id.get(session_id))
    }

    pub fn latest_request_id_for_session(&self, session_id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|row| row.session_id == session_id)
            .and_then(|row| clean_string(row.latest_request_id.as_deref()))
            .or_else(|| {
                self.requests_by_session_id
                    .get(session_id)
                    .and_then(|indexes| indexes.last().copied())
                    .map(|index| self.requests[index].request_id.clone())
            })
    }

    pub fn latest_runtime(&self, agent_did: &str) -> Option<&AgentRuntimeRow> {
        self.runtimes_by_agent_did
            .get(agent_did)
            .map(|index| &self.runtimes[*index])
    }

    pub fn latest_response_for_request(&self, request_id: &str) -> Option<&AgentResponseRow> {
        self.latest_response_by_request_id
            .get(request_id)
            .map(|index| &self.responses[*index])
    }

    pub fn row_count(&self) -> usize {
        self.agent_principals.len()
            + self.behaviors.len()
            + self.runtimes.len()
            + self.conversations.len()
            + self.requests.len()
            + self.responses.len()
            + self.messages.len()
            + self.sessions.len()
            + self.tool_calls.len()
            + self.tool_results.len()
            + self.compaction_entries.len()
            + self.scheduled_tasks.len()
            + self.tool_selections.len()
            + self.inference_backends.len()
            + self.inference_profiles.len()
            + self.tool_service_registries.len()
    }

    pub fn approx_serialized_bytes(&self) -> usize {
        serde_json::to_vec(&ClientStoreRows {
            agent_principals: self.agent_principals.clone(),
            behaviors: self.behaviors.clone(),
            runtimes: self.runtimes.clone(),
            conversations: self.conversations.clone(),
            requests: self.requests.clone(),
            responses: self.responses.clone(),
            messages: self.messages.clone(),
            sessions: self.sessions.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_results: self.tool_results.clone(),
            compaction_entries: self.compaction_entries.clone(),
            scheduled_tasks: self.scheduled_tasks.clone(),
            tool_selections: self.tool_selections.clone(),
            inference_backends: self.inference_backends.clone(),
            inference_profiles: self.inference_profiles.clone(),
            tool_service_registries: self.tool_service_registries.clone(),
        })
        .map(|bytes| bytes.len())
        .unwrap_or_default()
    }

    pub fn derive_turn(&self, session_id: &str) -> Option<ClientTurnState> {
        turns::derive_turn(self, session_id)
    }

    pub fn derive_turn_for_request(&self, request_id: &str) -> Option<ClientTurnState> {
        turns::derive_turn_for_request(self, request_id)
    }
}

pub type SharedClientStore = Arc<ClientStore>;
