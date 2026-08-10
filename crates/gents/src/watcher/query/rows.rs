use super::*;

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[derive(Clone, Deserialize)]
pub(super) struct AgentRequestRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) request_id: String,
    pub(super) agent_did: String,
    pub(super) requester_did: Option<String>,
    pub(super) behavior_id: Option<String>,
    pub(super) session_id: String,
    pub(super) content: String,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) top_k: Option<i64>,
    pub(super) max_tokens: Option<i64>,
    pub(super) metadata: Option<String>,
    pub(super) execution_origin: Option<String>,
    pub(super) created_at: String,
    pub(super) deadline: Option<String>,
    pub(super) subagent_depth: Option<u32>,
    pub(super) caused_by_parent_request_id: Option<String>,
    pub(super) caused_by_parent_request_doc_id: Option<String>,
    pub(super) caused_by_parent_tool_call_id: Option<String>,
    pub(super) caused_by_parent_tool_call_doc_id: Option<String>,
    pub(super) status: String,
    pub(super) lifecycle_state: Option<String>,
    pub(super) interrupt_requested_at: Option<String>,
    pub(super) valid_until: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SessionQueueRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) status: String,
    pub(super) lifecycle_state: Option<String>,
    pub(super) execution_origin: Option<String>,
    pub(super) metadata: Option<String>,
}

impl SessionQueueRow {
    pub(super) fn is_pending(&self) -> bool {
        self.status == "pending" && self.lifecycle_state.as_deref() == Some("pending")
    }

    pub(super) fn is_active_non_pending(&self) -> bool {
        !self.is_pending()
    }

    pub(super) fn is_deprecated_background_completion_wakeup(&self) -> bool {
        crate::lifecycle::queue::is_deprecated_background_completion_wakeup(
            self.execution_origin.as_deref(),
            self.metadata.as_deref(),
        )
    }
}

impl AgentRequestRow {
    pub(super) fn is_pending(&self) -> bool {
        self.status == "pending" && self.lifecycle_state.as_deref() == Some("pending")
    }

    pub(super) fn is_active_non_pending(&self) -> bool {
        !self.is_pending()
    }

    pub(super) fn has_preclaim_terminal_signal(&self) -> bool {
        if normalize_optional_string(self.interrupt_requested_at.clone()).is_some() {
            return true;
        }
        normalize_optional_string(self.valid_until.clone()).is_some_and(|value| {
            chrono::DateTime::parse_from_rfc3339(&value)
                .map(|dt| chrono::Utc::now() > dt.with_timezone(&chrono::Utc))
                .unwrap_or(false)
        })
    }

    pub(super) fn is_deprecated_background_completion_wakeup(&self) -> bool {
        crate::lifecycle::queue::is_deprecated_background_completion_wakeup(
            self.execution_origin.as_deref(),
            self.metadata.as_deref(),
        )
    }

    pub(super) fn into_agent_request(self) -> anyhow::Result<AgentRequest> {
        let req = AgentRequest {
            doc_id: self.doc_id,
            request_id: self.request_id,
            agent_did: self.agent_did,
            requester_did: normalize_optional_string(self.requester_did),
            behavior_id: normalize_optional_string(self.behavior_id),
            session_id: self.session_id,
            content: self.content,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            max_tokens: self.max_tokens,
            metadata: self.metadata,
            execution_origin: normalize_optional_string(self.execution_origin),
            created_at: self.created_at,
            deadline: normalize_optional_string(self.deadline),
            subagent_depth: self.subagent_depth.unwrap_or(0),
            caused_by_parent_request_id: self.caused_by_parent_request_id,
            caused_by_parent_request_doc_id: self.caused_by_parent_request_doc_id,
            caused_by_parent_tool_call_id: self.caused_by_parent_tool_call_id,
            caused_by_parent_tool_call_doc_id: self.caused_by_parent_tool_call_doc_id,
        };
        validate_agent_request_subagent_coherence(&req)?;
        Ok(req)
    }
}
