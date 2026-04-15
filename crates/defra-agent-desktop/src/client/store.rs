use std::collections::HashMap;
use std::sync::Arc;

use defra_agent_protocol::client_protocol::{
    derive_turn, AttemptView, ClientTurnState, RequestLifecycleState, RequestSnapshot,
    ResponseSnapshot, ResponseStatus,
};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentConversationRow, AgentMessageRow, AgentPrincipalRow, AgentRequestRow,
    AgentResponseRow, AgentRuntimeRow, AgentSessionRow, AgentToolCallRow, AgentToolResultRow,
    CompactionEntryRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow,
    ToolSelectionRow, ToolServiceRegistryRow,
};
use serde::Serialize;

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
    pub fn from_rows(mut rows: ClientStoreRows) -> Self {
        rows.conversations.sort_by(|left, right| {
            cmp_opt_str_desc(left.updated_at.as_deref(), right.updated_at.as_deref())
                .then_with(|| {
                    cmp_opt_str_desc(left.created_at.as_deref(), right.created_at.as_deref())
                })
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        rows.messages.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| {
                    left.sequence
                        .unwrap_or_default()
                        .cmp(&right.sequence.unwrap_or_default())
                })
                .then_with(|| {
                    cmp_opt_str_asc(left.timestamp.as_deref(), right.timestamp.as_deref())
                })
                .then_with(|| left.message_key.cmp(&right.message_key))
        });
        rows.requests.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| {
                    cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref())
                })
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        rows.tool_calls.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| {
                    left.message_sequence
                        .unwrap_or_default()
                        .cmp(&right.message_sequence.unwrap_or_default())
                })
                .then_with(|| {
                    cmp_opt_str_asc(left.started_at.as_deref(), right.started_at.as_deref())
                })
                .then_with(|| left.tool_call_key.cmp(&right.tool_call_key))
        });
        rows.tool_results.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| {
                    cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref())
                })
                .then_with(|| left.tool_name.cmp(&right.tool_name))
        });

        let mut conversations_by_agent_did = HashMap::new();
        for (index, row) in rows.conversations.iter().enumerate() {
            if let Some(agent_did) = row.agent_did.as_deref().filter(|value| !value.is_empty()) {
                conversations_by_agent_did
                    .entry(agent_did.to_owned())
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }

        let messages_by_session_id =
            build_vec_index(&rows.messages, |row| row.session_id.as_deref());
        let requests_by_session_id =
            build_vec_index(&rows.requests, |row| row.session_id.as_deref());
        let tool_calls_by_session_id =
            build_vec_index(&rows.tool_calls, |row| row.session_id.as_deref());
        let tool_results_by_session_id =
            build_vec_index(&rows.tool_results, |row| row.session_id.as_deref());

        let mut runtimes_by_agent_did = HashMap::new();
        for (index, row) in rows.runtimes.iter().enumerate() {
            runtimes_by_agent_did.insert(row.agent_did.clone(), index);
        }

        let mut latest_response_by_request_id = HashMap::new();
        for (index, row) in rows.responses.iter().enumerate() {
            let Some(request_id) = row.request_id.as_deref().filter(|value| !value.is_empty())
            else {
                continue;
            };

            match latest_response_by_request_id.get(request_id).copied() {
                Some(existing_index)
                    if compare_response_rows(
                        &rows.responses[index],
                        &rows.responses[existing_index],
                    )
                    .is_gt() =>
                {
                    latest_response_by_request_id.insert(request_id.to_owned(), index);
                }
                None => {
                    latest_response_by_request_id.insert(request_id.to_owned(), index);
                }
                _ => {}
            }
        }

        Self {
            agent_principals: rows.agent_principals,
            behaviors: rows.behaviors,
            runtimes: rows.runtimes,
            conversations: rows.conversations,
            requests: rows.requests,
            responses: rows.responses,
            messages: rows.messages,
            sessions: rows.sessions,
            tool_calls: rows.tool_calls,
            tool_results: rows.tool_results,
            compaction_entries: rows.compaction_entries,
            scheduled_tasks: rows.scheduled_tasks,
            tool_selections: rows.tool_selections,
            inference_backends: rows.inference_backends,
            inference_profiles: rows.inference_profiles,
            tool_service_registries: rows.tool_service_registries,
            conversations_by_agent_did,
            messages_by_session_id,
            requests_by_session_id,
            tool_calls_by_session_id,
            tool_results_by_session_id,
            runtimes_by_agent_did,
            latest_response_by_request_id,
        }
    }

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
        let attempts: Vec<_> = self
            .requests_by_session_id
            .get(session_id)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .filter_map(|index| self.attempt_for_request(*index))
            .collect();

        derive_turn(&attempts)
    }

    fn attempt_for_request(&self, index: usize) -> Option<AttemptView> {
        let row = &self.requests[index];
        let lifecycle =
            RequestLifecycleState::try_from(row.lifecycle_state.as_deref().unwrap_or_default())
                .ok()?;

        let response = self
            .latest_response_by_request_id
            .get(&row.request_id)
            .and_then(|response_index| response_status(&self.responses[*response_index]))
            .map(|status| ResponseSnapshot { status });

        Some(AttemptView {
            request: RequestSnapshot {
                request_id: row.request_id.clone(),
                retry_parent_request: clean_string(row.retry_parent_request.as_deref()),
                lifecycle_state: lifecycle,
                is_superseded: clean_string(row.superseded_by_request.as_deref()).is_some(),
            },
            response,
        })
    }
}

pub type SharedClientStore = Arc<ClientStore>;

fn build_vec_index<T>(
    rows: &[T],
    key_fn: impl Fn(&T) -> Option<&str>,
) -> HashMap<String, Vec<usize>> {
    let mut index = HashMap::new();
    for (row_index, row) in rows.iter().enumerate() {
        if let Some(key) = clean_string(key_fn(row)) {
            index.entry(key).or_insert_with(Vec::new).push(row_index);
        }
    }
    index
}

fn indexes_to_refs<'a, T>(rows: &'a [T], indexes: Option<&Vec<usize>>) -> Vec<&'a T> {
    indexes
        .into_iter()
        .flat_map(|indexes| indexes.iter())
        .map(|index| &rows[*index])
        .collect()
}

fn response_status(row: &AgentResponseRow) -> Option<ResponseStatus> {
    match row.status.as_deref().unwrap_or_default() {
        "streaming" => Some(ResponseStatus::Streaming),
        "complete" | "completed" => Some(ResponseStatus::Complete),
        "error" | "failed" => Some(ResponseStatus::Error),
        _ => None,
    }
}

fn compare_response_rows(left: &AgentResponseRow, right: &AgentResponseRow) -> std::cmp::Ordering {
    left.progress_seq
        .unwrap_or_default()
        .cmp(&right.progress_seq.unwrap_or_default())
        .then_with(|| cmp_opt_str_asc(left.completed_at.as_deref(), right.completed_at.as_deref()))
        .then_with(|| cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref()))
        .then_with(|| left.response_key.cmp(&right.response_key))
}

fn cmp_opt_str_desc(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    right.unwrap_or_default().cmp(left.unwrap_or_default())
}

fn cmp_opt_str_asc(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    left.unwrap_or_default().cmp(right.unwrap_or_default())
}

fn clean_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
