use super::*;

impl ClientStore {
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

    pub fn transcript_for_agent(&self, session_id: &str, agent_did: &str) -> TranscriptView<'_> {
        let message_indexes = self
            .messages_by_session_id
            .get(session_id)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .copied()
            .filter(|index| {
                source_agent_matches(&self.message_source_agent_dids, *index, agent_did)
            })
            .collect::<Vec<_>>();
        let tool_call_indexes = self
            .tool_calls_by_session_id
            .get(session_id)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .copied()
            .filter(|index| {
                source_agent_matches(&self.tool_call_source_agent_dids, *index, agent_did)
            })
            .collect::<Vec<_>>();
        let tool_result_indexes = self
            .tool_results_by_session_id
            .get(session_id)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .copied()
            .filter(|index| {
                let row = &self.tool_results[*index];
                row_agent_matches(row.agent_did.as_deref(), agent_did)
                    && source_agent_matches(&self.tool_result_source_agent_dids, *index, agent_did)
            })
            .collect::<Vec<_>>();

        TranscriptView {
            messages: message_indexes
                .into_iter()
                .map(|index| &self.messages[index])
                .collect(),
            tool_calls: tool_call_indexes
                .into_iter()
                .map(|index| &self.tool_calls[index])
                .collect(),
            tool_results: tool_result_indexes
                .into_iter()
                .map(|index| &self.tool_results[index])
                .collect(),
        }
    }

    pub fn requests_for_session(&self, session_id: &str) -> Vec<&AgentRequestRow> {
        indexes_to_refs(&self.requests, self.requests_by_session_id.get(session_id))
    }

    pub fn requests_for_session_for_agent(
        &self,
        session_id: &str,
        agent_did: &str,
    ) -> Vec<&AgentRequestRow> {
        self.requests_for_session(session_id)
            .into_iter()
            .filter(|row| row_agent_matches(row.agent_did.as_deref(), agent_did))
            .collect()
    }

    pub fn latest_request_id_for_session(&self, session_id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|row| row.session_id == session_id)
            .and_then(|row| clean_string(row.latest_request_id.as_deref()))
            .or_else(|| {
                self.requests_by_session_id
                    .get(session_id)
                    .and_then(|indexes| indexes.last())
                    .copied()
                    .map(|index| self.requests[index].request_id.clone())
            })
    }

    pub fn latest_request_id_for_session_for_agent(
        &self,
        session_id: &str,
        agent_did: &str,
    ) -> Option<String> {
        self.conversations
            .iter()
            .find(|row| row.session_id == session_id && row.agent_did.as_deref() == Some(agent_did))
            .and_then(|row| clean_string(row.latest_request_id.as_deref()))
            .or_else(|| {
                self.requests_by_session_id
                    .get(session_id)
                    .and_then(|indexes| {
                        indexes.iter().rev().find(|index| {
                            row_agent_matches(
                                self.requests[**index].agent_did.as_deref(),
                                agent_did,
                            )
                        })
                    })
                    .map(|index| self.requests[*index].request_id.clone())
            })
    }

    pub fn latest_runtime(&self, agent_did: &str) -> Option<&AgentRuntimeRow> {
        self.runtimes_by_agent_did
            .get(agent_did)
            .map(|index| &self.runtimes[*index])
    }

    pub fn behavior_readiness(&self, agent_did: &str) -> Option<&AgentBehaviorReadinessRow> {
        self.behavior_readiness_by_agent_did
            .get(agent_did)
            .map(|index| &self.behavior_readiness[*index])
    }

    pub fn latest_response_for_request(&self, request_id: &str) -> Option<&AgentResponseRow> {
        self.latest_response_by_request_id
            .get(request_id)
            .map(|index| &self.responses[*index])
    }

    pub fn latest_response_for_request_for_agent(
        &self,
        request_id: &str,
        agent_did: &str,
    ) -> Option<&AgentResponseRow> {
        self.responses
            .iter()
            .filter(|row| {
                row.request_id.as_deref() == Some(request_id)
                    && row_agent_matches(row.agent_did.as_deref(), agent_did)
            })
            .max_by(|left, right| {
                left.progress_seq
                    .unwrap_or_default()
                    .cmp(&right.progress_seq.unwrap_or_default())
                    .then_with(|| {
                        left.completed_at
                            .as_deref()
                            .unwrap_or_default()
                            .cmp(right.completed_at.as_deref().unwrap_or_default())
                    })
                    .then_with(|| {
                        left.created_at
                            .as_deref()
                            .unwrap_or_default()
                            .cmp(right.created_at.as_deref().unwrap_or_default())
                    })
                    .then_with(|| left.response_key.cmp(&right.response_key))
            })
    }

    pub fn request_row(&self, request_id: &str) -> Option<&AgentRequestRow> {
        self.request_index_by_id
            .get(request_id)
            .map(|index| &self.requests[*index])
    }

    pub fn mailbox_items_for_requester(&self, requester_did: &str) -> Vec<&MailboxItemRow> {
        self.mailbox_items
            .iter()
            .filter(|row| row.requester_did == requester_did)
            .collect()
    }

    pub fn row_count(&self) -> usize {
        self.agent_principals.len()
            + self.behaviors.len()
            + self.runtimes.len()
            + self.behavior_readiness.len()
            + self.conversations.len()
            + self.requests.len()
            + self.mailbox_items.len()
            + self.responses.len()
            + self.messages.len()
            + self.sessions.len()
            + self.goals.len()
            + self.tool_calls.len()
            + self.tool_results.len()
            + self.compaction_entries.len()
            + self.tasks.len()
            + self.schedules.len()
            + self.event_triggers.len()
            + self.skills.len()
            + self.tool_selections.len()
            + self.inference_backends.len()
            + self.inference_profiles.len()
            + self.tool_service_registries.len()
    }

    pub fn approx_serialized_bytes(&self) -> usize {
        serde_json::to_vec(&self.to_rows())
            .map(|bytes| bytes.len())
            .unwrap_or_default()
    }

    pub fn derive_turn(&self, session_id: &str) -> Option<ClientTurnState> {
        turns::derive_turn(self, session_id)
    }

    pub fn derive_turn_for_agent(
        &self,
        session_id: &str,
        agent_did: &str,
    ) -> Option<ClientTurnState> {
        turns::derive_turn_for_agent(self, session_id, agent_did)
    }

    pub fn derive_turn_for_request(&self, request_id: &str) -> Option<ClientTurnState> {
        turns::derive_turn_for_request(self, request_id)
    }

    pub fn derive_turn_for_request_for_agent(
        &self,
        request_id: &str,
        agent_did: &str,
    ) -> Option<ClientTurnState> {
        turns::derive_turn_for_request_for_agent(self, request_id, agent_did)
    }
}
