use crate::request_admission::RequestPurpose;
use crate::request_input::{QueueSource, RequestInput};

use super::AgentRequestRow;

pub const AGENT_REQUEST_FIELDS: &str = r#"
                    _docID
                    request_id
                    purpose
                    node_did
                    requester_did
                    agent_did
                    session_id
                    content
                    retry_parent_request_doc_id
                    max_total_tokens
                    input
                    execution_origin
                    created_at
                    deadline
                    execution_generation
                    execution_lease_secs
                    execution_lease_expires_at
                    request_hop
                    caused_by_parent_request_id
                    caused_by_parent_request_doc_id
                    caused_by_parent_tool_call_id
                    caused_by_parent_tool_call_doc_id
                    caused_by_trigger_id
                    caused_by_trigger_kind
                    caused_by_source_doc_id
                    caused_by_correlation
                    caused_by_trigger_context
                    workspace_id
                    workspace_owner_node_did
                    workspace_authority
                    workspace_seal_hash
"#;

#[derive(Debug, thiserror::Error)]
pub enum AgentRequestRowError {
    #[error("agent request request_hop must fit in u32")]
    RequestHopOutOfRange(#[source] std::num::TryFromIntError),
    #[error("agent request execution_lease_secs must be non-negative")]
    NegativeExecutionLease,
    #[error("agent request is missing {0}")]
    MissingField(&'static str),
    #[error("agent request agent_did must be a canonical nonblank identifier")]
    NoncanonicalAgentDid,
    #[error("agent request max_total_tokens must be non-negative")]
    NegativeMaxTotalTokens,
    #[error("auxiliary request requires parent-only provenance, not delegated authority")]
    AuxiliaryProvenance,
    #[error("AgentRequest parent linkage incoherent: must set both or neither parent fields")]
    ParentLinkageIncoherent,
}

#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub purpose: RequestPurpose,
    pub doc_id: String,
    pub request_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
    pub agent_did: String,
    pub session_id: String,
    pub content: String,
    pub retry_parent_request_doc_id: Option<String>,
    pub max_total_tokens: Option<i64>,
    pub input: RequestInput,
    pub execution_origin: Option<String>,
    pub created_at: String,
    pub deadline: Option<String>,
    pub execution_generation: Option<String>,
    pub execution_lease_secs: Option<i64>,
    pub execution_lease_expires_at: Option<String>,
    pub request_hop: u32,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_request_doc_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    pub caused_by_trigger_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
    pub caused_by_source_doc_id: Option<String>,
    pub caused_by_correlation: Option<String>,
    pub caused_by_trigger_context: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_owner_node_did: Option<String>,
    pub workspace_authority: Option<String>,
    pub workspace_seal_hash: Option<String>,
}

impl AgentRequest {
    pub fn has_automated_trigger_lineage(&self) -> bool {
        let has_trigger_id = self
            .caused_by_trigger_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        has_trigger_id
            && matches!(
                self.caused_by_trigger_kind.as_deref().map(str::trim),
                Some("event" | "schedule")
            )
    }
}

impl TryFrom<AgentRequestRow> for AgentRequest {
    type Error = AgentRequestRowError;

    fn try_from(row: AgentRequestRow) -> Result<Self, Self::Error> {
        let request_hop = row
            .request_hop
            .map(u32::try_from)
            .transpose()
            .map_err(AgentRequestRowError::RequestHopOutOfRange)?
            .unwrap_or(0);
        let execution_lease_secs = match row.execution_lease_secs {
            Some(secs) if secs < 0 => return Err(AgentRequestRowError::NegativeExecutionLease),
            other => other,
        };
        let request = Self {
            purpose: row
                .purpose
                .ok_or(AgentRequestRowError::MissingField("purpose"))?,
            doc_id: row
                .doc_id
                .ok_or(AgentRequestRowError::MissingField("_docID"))?,
            request_id: row.request_id,
            node_did: row
                .node_did
                .ok_or(AgentRequestRowError::MissingField("node_did"))?,
            requester_did: normalize_optional_string(row.requester_did),
            agent_did: row
                .agent_did
                .ok_or(AgentRequestRowError::MissingField("agent_did"))?,
            session_id: row
                .session_id
                .ok_or(AgentRequestRowError::MissingField("session_id"))?,
            content: row
                .content
                .ok_or(AgentRequestRowError::MissingField("content"))?,
            retry_parent_request_doc_id: normalize_optional_string(row.retry_parent_request_doc_id),
            max_total_tokens: row.max_total_tokens,
            input: row.input.unwrap_or_default(),
            execution_origin: normalize_optional_string(row.execution_origin),
            created_at: row
                .created_at
                .ok_or(AgentRequestRowError::MissingField("created_at"))?,
            deadline: normalize_optional_string(row.deadline),
            execution_generation: normalize_optional_string(row.execution_generation),
            execution_lease_secs,
            execution_lease_expires_at: normalize_optional_string(row.execution_lease_expires_at),
            request_hop,
            caused_by_parent_request_id: row.caused_by_parent_request_id,
            caused_by_parent_request_doc_id: row.caused_by_parent_request_doc_id,
            caused_by_parent_tool_call_id: row.caused_by_parent_tool_call_id,
            caused_by_parent_tool_call_doc_id: row.caused_by_parent_tool_call_doc_id,
            caused_by_trigger_id: normalize_optional_string(row.caused_by_trigger_id),
            caused_by_trigger_kind: normalize_optional_string(row.caused_by_trigger_kind),
            caused_by_source_doc_id: normalize_optional_string(row.caused_by_source_doc_id),
            caused_by_correlation: normalize_optional_string(row.caused_by_correlation),
            caused_by_trigger_context: normalize_optional_string(row.caused_by_trigger_context),
            workspace_id: normalize_optional_string(row.workspace_id),
            workspace_owner_node_did: row.workspace_owner_node_did,
            workspace_authority: normalize_optional_string(row.workspace_authority),
            workspace_seal_hash: normalize_optional_string(row.workspace_seal_hash),
        };
        validate_agent_request(&request)?;
        Ok(request)
    }
}

pub fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub fn validate_agent_request(req: &AgentRequest) -> Result<(), AgentRequestRowError> {
    if req.agent_did.is_empty() || req.agent_did.trim() != req.agent_did {
        return Err(AgentRequestRowError::NoncanonicalAgentDid);
    }
    if req.max_total_tokens.is_some_and(|limit| limit < 0) {
        return Err(AgentRequestRowError::NegativeMaxTotalTokens);
    }
    let has_parent_req = req.caused_by_parent_request_id.is_some();
    let has_parent_tc = req.caused_by_parent_tool_call_id.is_some();
    let has_parent_req_doc = req.caused_by_parent_request_doc_id.is_some();
    let has_parent_tc_doc = req.caused_by_parent_tool_call_doc_id.is_some();
    let auxiliary_parent_link = req.purpose.is_auxiliary();
    if auxiliary_parent_link
        && !(has_parent_req
            && has_parent_req_doc
            && !has_parent_tc
            && !has_parent_tc_doc
            && req.request_hop == 0)
    {
        return Err(AgentRequestRowError::AuxiliaryProvenance);
    }
    let request_only_control_link = has_parent_req
        && !has_parent_tc
        && (auxiliary_parent_link
            || req.input.queue.as_ref().is_some_and(|queue| {
                matches!(
                    queue.source,
                    QueueSource::User
                        | QueueSource::Steering
                        | QueueSource::Goal
                        | QueueSource::BackgroundCompletion
                )
            }));
    if has_parent_req != has_parent_req_doc || has_parent_tc != has_parent_tc_doc {
        return Err(AgentRequestRowError::ParentLinkageIncoherent);
    }
    if has_parent_req != has_parent_tc && !request_only_control_link {
        return Err(AgentRequestRowError::ParentLinkageIncoherent);
    }
    if req.request_hop > 0
        && !request_only_control_link
        && !(has_parent_req && has_parent_tc && has_parent_req_doc && has_parent_tc_doc)
    {
        return Err(AgentRequestRowError::ParentLinkageIncoherent);
    }
    let is_top_level = !has_parent_req;
    if is_top_level && req.request_hop != 0 {
        return Err(AgentRequestRowError::ParentLinkageIncoherent);
    }
    if !is_top_level && req.request_hop == 0 && !request_only_control_link {
        return Err(AgentRequestRowError::ParentLinkageIncoherent);
    }
    Ok(())
}
