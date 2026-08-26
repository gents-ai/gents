use super::*;

impl ClientStore {
    pub(crate) fn into_observer_projection(mut self) -> Self {
        self.messages.clear();
        self.message_source_agent_dids.clear();
        self.messages_by_session_id.clear();
        self.tool_calls.clear();
        self.tool_call_source_agent_dids.clear();
        self.tool_calls_by_session_id.clear();
        self.tool_results.clear();
        self.tool_result_source_agent_dids.clear();
        self.tool_results_by_session_id.clear();
        self.compaction_entries.clear();
        self.compaction_entry_source_agent_dids.clear();
        self
    }

    /// Merge an observer patch known to contain only `AgentResponse` rows.
    ///
    /// The normal immutable merge rebuilds every collection and every index.
    /// Streaming responses are the hot exception: their patch cannot affect
    /// transcript/config indexes, so update the response vector and its two
    /// indexes without cloning historical messages, tools, or control-plane
    /// rows. `ObservedStore` owns snapshot isolation with `Arc::make_mut`.
    pub(crate) fn merge_response_patch_in_place(&mut self, patch: ClientStore) {
        for incoming in patch.responses {
            let key = response_merge_key(&incoming);
            let index = if let Some(index) = self.response_index_by_key.get(&key).copied() {
                let previous_request_id = self.responses[index].request_id.clone();
                self.responses[index] = incoming;
                if previous_request_id != self.responses[index].request_id {
                    if previous_request_id.as_deref().is_some_and(|request_id| {
                        self.latest_response_by_request_id.get(request_id) == Some(&index)
                    }) {
                        self.reindex_latest_response_for_request(
                            previous_request_id.as_deref().unwrap_or_default(),
                        );
                    }
                }
                index
            } else {
                let index = self.responses.len();
                self.responses.push(incoming);
                self.response_index_by_key.insert(key, index);
                index
            };

            let Some(request_id) = self.responses[index]
                .request_id
                .as_deref()
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            match self.latest_response_by_request_id.get(request_id).copied() {
                Some(current) if current != index => {
                    if indexing::compare_response_rows(
                        &self.responses[index],
                        &self.responses[current],
                    )
                    .is_gt()
                    {
                        self.latest_response_by_request_id
                            .insert(request_id.to_owned(), index);
                    }
                }
                _ => {
                    self.latest_response_by_request_id
                        .insert(request_id.to_owned(), index);
                }
            }
        }
    }

    fn reindex_latest_response_for_request(&mut self, request_id: &str) {
        let latest = self
            .responses
            .iter()
            .enumerate()
            .filter(|(_, row)| row.request_id.as_deref() == Some(request_id))
            .max_by(|(_, left), (_, right)| indexing::compare_response_rows(left, right))
            .map(|(index, _)| index);
        if let Some(index) = latest {
            self.latest_response_by_request_id
                .insert(request_id.to_owned(), index);
        } else {
            self.latest_response_by_request_id.remove(request_id);
        }
    }

    pub(crate) fn is_response_only_patch(&self) -> bool {
        self.row_count() == self.responses.len()
    }
}
