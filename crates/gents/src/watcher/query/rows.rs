use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::*;

pub(super) const BACKGROUND_COMPLETION_AGING_THRESHOLD: chrono::Duration =
    chrono::Duration::seconds(30);

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

/// Result of [`AgentRequestRow::preclaim_signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreclaimSignal {
    /// No terminal or malformed signal; ordinary session-based claimability
    /// applies.
    None,
    /// Interrupted, or `valid_until` has expired: claim immediately so the
    /// row can be terminalized.
    Terminal,
    /// `valid_until` is present but did not parse: never claimable.
    Malformed,
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
    pub(super) seed: Option<i64>,
    pub(super) max_tokens: Option<i64>,
    pub(super) max_total_tokens: Option<i64>,
    pub(super) metadata: Option<String>,
    pub(super) execution_origin: Option<String>,
    pub(super) created_at: String,
    pub(super) deadline: Option<String>,
    pub(super) subagent_depth: Option<u32>,
    pub(super) caused_by_parent_request_id: Option<String>,
    pub(super) caused_by_parent_request_doc_id: Option<String>,
    pub(super) caused_by_parent_tool_call_id: Option<String>,
    pub(super) caused_by_parent_tool_call_doc_id: Option<String>,
    pub(super) caused_by_trigger_id: Option<String>,
    pub(super) caused_by_trigger_kind: Option<String>,
    pub(super) caused_by_source_doc_id: Option<String>,
    pub(super) caused_by_correlation: Option<String>,
    pub(super) caused_by_trigger_context: Option<String>,
    #[serde(default)]
    pub(super) workspace_id: Option<String>,
    #[serde(default)]
    pub(super) workspace_authority: Option<String>,
    #[serde(default)]
    pub(super) workspace_owner_deployment_id: Option<String>,
    #[serde(default)]
    pub(super) workspace_seal_hash: Option<String>,
    #[serde(default)]
    pub(super) lifecycle_state: Option<RequestLifecycleState>,
    pub(super) interrupt_requested_at: Option<String>,
    pub(super) valid_until: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SessionQueueRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    #[serde(default)]
    pub(super) lifecycle_state: Option<RequestLifecycleState>,
}

impl SessionQueueRow {
    pub(super) fn is_pending(&self) -> bool {
        self.lifecycle_state == Some(RequestLifecycleState::Pending)
    }
}

impl AgentRequestRow {
    pub(super) fn is_pending(&self) -> bool {
        self.lifecycle_state == Some(RequestLifecycleState::Pending)
    }

    /// Pre-claim disposition from `interrupt_requested_at` and `valid_until`,
    /// checked before the watcher issues any claim-scoped query.
    pub(super) fn preclaim_signal(&self) -> PreclaimSignal {
        if normalize_optional_string(self.interrupt_requested_at.clone()).is_some() {
            return PreclaimSignal::Terminal;
        }
        match crate::lifecycle::parse_valid_until(self.valid_until.as_deref(), chrono::Utc::now()) {
            crate::lifecycle::TtlOutcome::Expired(_) => PreclaimSignal::Terminal,
            // Fail closed: an unparseable TTL is not evidence the request is
            // still live, so it must not be claimed as if unset.
            crate::lifecycle::TtlOutcome::Malformed(_) => PreclaimSignal::Malformed,
            crate::lifecycle::TtlOutcome::NotSet | crate::lifecycle::TtlOutcome::Live(_) => {
                PreclaimSignal::None
            }
        }
    }

    pub(super) fn is_aged_background_completion_wakeup(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        if self.execution_origin.as_deref() != Some("scheduled")
            || !crate::lifecycle::queue::is_automated_wakeup(self.metadata.as_deref())
        {
            return false;
        }
        chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map(|created_at| {
                now.signed_duration_since(created_at.with_timezone(&chrono::Utc))
                    >= BACKGROUND_COMPLETION_AGING_THRESHOLD
            })
            .unwrap_or(false)
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
            seed: self.seed,
            max_tokens: self.max_tokens,
            max_total_tokens: self.max_total_tokens,
            metadata: self.metadata,
            execution_origin: normalize_optional_string(self.execution_origin),
            created_at: self.created_at,
            deadline: normalize_optional_string(self.deadline),
            subagent_depth: self.subagent_depth.unwrap_or(0),
            caused_by_parent_request_id: self.caused_by_parent_request_id,
            caused_by_parent_request_doc_id: self.caused_by_parent_request_doc_id,
            caused_by_parent_tool_call_id: self.caused_by_parent_tool_call_id,
            caused_by_parent_tool_call_doc_id: self.caused_by_parent_tool_call_doc_id,
            caused_by_trigger_id: normalize_optional_string(self.caused_by_trigger_id),
            caused_by_trigger_kind: normalize_optional_string(self.caused_by_trigger_kind),
            caused_by_source_doc_id: normalize_optional_string(self.caused_by_source_doc_id),
            caused_by_correlation: normalize_optional_string(self.caused_by_correlation),
            caused_by_trigger_context: normalize_optional_string(self.caused_by_trigger_context),
            workspace_id: normalize_optional_string(self.workspace_id),
            workspace_authority: normalize_optional_string(self.workspace_authority),
            workspace_owner_deployment_id: normalize_optional_string(
                self.workspace_owner_deployment_id,
            ),
            workspace_seal_hash: normalize_optional_string(self.workspace_seal_hash),
        };
        validate_agent_request(&req)?;
        Ok(req)
    }
}
