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

    pub fn transcript_for_node(&self, session_id: &str, node_did: &str) -> TranscriptView<'_> {
        let message_indexes = self
            .transcript_messages_by_session_id
            .get(session_id)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .copied()
            .filter(|index| self.transcript_messages[*index].message.node_did == node_did)
            .collect::<Vec<_>>();
        let tool_call_indexes = self
            .tool_calls_by_session_id
            .get(session_id)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .copied()
            .filter(|index| source_node_matches(&self.tool_call_source_node_dids, *index, node_did))
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
                    row.segment.session_id == session_id && row.segment.node_did == node_did
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

    pub fn requests_for_session_for_node(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Vec<&AgentRequestRow> {
        self.requests_for_session(session_id)
            .into_iter()
            .filter(|row| row_node_matches(row.node_did.as_deref(), node_did))
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

    pub fn latest_request_id_for_session_for_node(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Option<String> {
        if let Some(latest) = self
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.node_did == node_did)
            .and_then(|row| row.observation.as_ref())
            .and_then(|observation| observation.latest_request.as_ref())
        {
            return self
                .requests
                .iter()
                .find(|request| {
                    request.request_id == latest.request_id
                        && request.doc_id.as_deref() == Some(latest.request_doc_id.as_str())
                        && row_node_matches(request.node_did.as_deref(), node_did)
                })
                .map(|request| request.request_id.clone());
        }
        self.requests_by_session_id
            .get(session_id)
            .and_then(|indexes| {
                indexes.iter().rev().find(|index| {
                    row_node_matches(self.requests[**index].node_did.as_deref(), node_did)
                })
            })
            .map(|index| self.requests[*index].request_id.clone())
    }

    /// The physical request whose tip a session read loads: the turn reached
    /// from `request_id` (`turns::session_turn_request`) within that exact
    /// node and requester scope. A queued or folded submission names the
    /// running request, whose open output is the live tail.
    pub fn session_tip_request(
        &self,
        session_id: &str,
        node_did: Option<&str>,
        requester_did: Option<&str>,
        request_id: &str,
    ) -> Option<AgentRequestRow> {
        let requests = self
            .requests_for_session(session_id)
            .into_iter()
            .filter(|row| {
                row.node_did.as_deref() == node_did && row.requester_did.as_deref() == requester_did
            })
            .collect::<Vec<_>>();
        let submitted = requests
            .iter()
            .copied()
            .find(|row| row.request_id == request_id)?;
        Some(turns::session_turn_request(&requests, submitted).clone())
    }

    /// The request whose turn the session is on (`turns::session_turn_request`),
    /// starting from its newest request.
    pub fn turn_request_id_for_session(&self, session_id: &str) -> Option<String> {
        let requests = self.requests_for_session(session_id);
        self.turn_request_id(&requests, self.latest_request_id_for_session(session_id)?)
    }

    pub fn turn_request_id_for_session_for_node(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Option<String> {
        let requests = self.requests_for_session_for_node(session_id, node_did);
        self.turn_request_id(
            &requests,
            self.latest_request_id_for_session_for_node(session_id, node_did)?,
        )
    }

    fn turn_request_id(&self, requests: &[&AgentRequestRow], newest: String) -> Option<String> {
        let Some(newest_row) = requests.iter().find(|row| row.request_id == newest) else {
            return Some(newest);
        };
        Some(
            turns::session_turn_request(requests, newest_row)
                .request_id
                .clone(),
        )
    }

    pub fn latest_runtime(&self, node_did: &str) -> Option<&NodeRuntimeRow> {
        self.runtimes_by_node_did
            .get(node_did)
            .map(|index| &self.runtimes[*index])
    }

    pub fn node_readiness(&self, node_did: &str) -> Option<&NodeReadinessRow> {
        self.node_readiness_by_node_did
            .get(node_did)
            .map(|index| &self.node_readiness[*index])
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
        self.nodes.len()
            + self.agents.len()
            + self.runtimes.len()
            + self.node_readiness.len()
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

    pub fn derive_turn_for_node(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Option<ClientTurnState> {
        turns::derive_turn_for_node(self, session_id, node_did)
    }

    pub fn derive_turn_for_request(&self, request_id: &str) -> Option<ClientTurnState> {
        turns::derive_turn_for_request(self, request_id)
    }

    pub fn derive_turn_for_request_for_node(
        &self,
        request_id: &str,
        node_did: &str,
    ) -> Option<ClientTurnState> {
        turns::derive_turn_for_request_for_node(self, request_id, node_did)
    }
}
