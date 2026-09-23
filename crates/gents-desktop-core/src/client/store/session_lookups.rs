use super::*;

impl ClientStore {
    pub fn transcript(&self, session_id: &str) -> TranscriptView<'_> {
        TranscriptView {
            messages: indexes_to_refs(
                &self.transcript_messages,
                self.transcript_messages_by_session_id.get(session_id),
            ),
            output_segments: self
                .output_segments
                .iter()
                .filter(|row| row.segment.session_id == session_id)
                .collect(),
            tool_calls: indexes_to_refs(
                &self.tool_calls,
                self.tool_calls_by_session_id.get(session_id),
            ),
        }
    }

    pub fn transcript_for_agent(&self, session_id: &str, agent_did: &str) -> TranscriptView<'_> {
        let message_indexes = self
            .transcript_messages_by_session_id
            .get(session_id)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .copied()
            .filter(|index| self.transcript_messages[*index].message.agent_did == agent_did)
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
        TranscriptView {
            messages: message_indexes
                .into_iter()
                .map(|index| &self.transcript_messages[index])
                .collect(),
            output_segments: self
                .output_segments
                .iter()
                .filter(|row| {
                    row.segment.session_id == session_id && row.segment.agent_did == agent_did
                })
                .collect(),
            tool_calls: tool_call_indexes
                .into_iter()
                .map(|index| &self.tool_calls[index])
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
        if let Some(latest) = self
            .sessions
            .iter()
            .find(|row| row.session_id == session_id)
            .and_then(|row| row.observation.as_ref())
            .and_then(|observation| observation.latest_request.as_ref())
        {
            return self
                .requests
                .iter()
                .find(|request| {
                    request.request_id == latest.request_id
                        && request.doc_id.as_deref() == Some(latest.request_doc_id.as_str())
                })
                .map(|request| request.request_id.clone());
        }
        self.requests_by_session_id
            .get(session_id)
            .and_then(|indexes| indexes.last())
            .copied()
            .map(|index| self.requests[index].request_id.clone())
    }

    pub fn latest_request_id_for_session_for_agent(
        &self,
        session_id: &str,
        agent_did: &str,
    ) -> Option<String> {
        if let Some(latest) = self
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.agent_did == agent_did)
            .and_then(|row| row.observation.as_ref())
            .and_then(|observation| observation.latest_request.as_ref())
        {
            return self
                .requests
                .iter()
                .find(|request| {
                    request.request_id == latest.request_id
                        && request.doc_id.as_deref() == Some(latest.request_doc_id.as_str())
                        && row_agent_matches(request.agent_did.as_deref(), agent_did)
                })
                .map(|request| request.request_id.clone());
        }
        self.requests_by_session_id
            .get(session_id)
            .and_then(|indexes| {
                indexes.iter().rev().find(|index| {
                    row_agent_matches(self.requests[**index].agent_did.as_deref(), agent_did)
                })
            })
            .map(|index| self.requests[*index].request_id.clone())
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
            + self.requests.len()
            + self.mailbox_items.len()
            + self.transcript_messages.len()
            + self.output_segments.len()
            + self.sessions.len()
            + self.goals.len()
            + self.tool_calls.len()
            + self.compaction_entries.len()
            + self.tasks.len()
            + self.schedules.len()
            + self.triggers.len()
            + self.trigger_observations.len()
            + self.skills.len()
            + self.tools.len()
            + self.inference_backends.len()
            + self.inference_profiles.len()
            + self.tool_service_registries.len()
    }

    pub fn approx_serialized_bytes(&self) -> usize {
        let control_plane = serde_json::to_vec(&self.to_rows())
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        let canonical = self
            .transcript_messages
            .iter()
            .map(|row| row.doc_id.len() + serde_json::to_vec(&row.message).map_or(0, |v| v.len()))
            .chain(self.output_segments.iter().map(|row| {
                row.doc_id.len() + serde_json::to_vec(&row.segment).map_or(0, |v| v.len())
            }))
            .sum::<usize>();
        control_plane.saturating_add(canonical)
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
