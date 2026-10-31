use super::*;

pub(crate) const GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS: &str = r#"
    request_id purpose node_did requester_did agent_id session_id
    retry_parent_request retry_parent_request_doc_id retry_root_request retry_key
    content input
    execution_origin caused_by_trigger_id caused_by_trigger_doc_id
    caused_by_trigger_kind caused_by_correlation caused_by_trigger_context
    caused_by_source_doc_id retry_count max_retries valid_until request_hop
    caused_by_parent_request_id caused_by_parent_request_doc_id
    caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id
    workspace_id workspace_owner_node_did workspace_authority workspace_seal_hash
    admission_kind admission_signer_did enrollment_request_id
    enrollment_request_digest enrollment_admin_did
    enrollment_authorization_sequence enrollment_authorization_expires_at
    runtime_issuer_did runtime_source_request_id runtime_source_kind
"#;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct GoalBackedRequestFingerprint {
    request_id: String,
    purpose: gents_protocol::request_admission::RequestPurpose,
    node_did: String,
    requester_did: String,
    agent_id: String,
    session_id: String,
    retry_parent_request: Option<String>,
    retry_parent_request_doc_id: Option<String>,
    retry_root_request: Option<String>,
    retry_key: Option<String>,
    content: String,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null"
    )]
    input: gents_protocol::request_input::RequestInput,
    execution_origin: String,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_doc_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    caused_by_correlation: Option<String>,
    caused_by_trigger_context: Option<String>,
    caused_by_source_doc_id: Option<String>,
    retry_count: i64,
    max_retries: i64,
    valid_until: Option<String>,
    request_hop: i64,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
    workspace_id: Option<String>,
    workspace_owner_node_did: Option<String>,
    workspace_authority: Option<String>,
    workspace_seal_hash: Option<String>,
    admission_kind: String,
    admission_signer_did: String,
    enrollment_request_id: Option<String>,
    enrollment_request_digest: Option<String>,
    enrollment_admin_did: Option<String>,
    enrollment_authorization_sequence: Option<i64>,
    enrollment_authorization_expires_at: Option<String>,
    runtime_issuer_did: Option<String>,
    runtime_source_request_id: Option<String>,
    runtime_source_kind: Option<String>,
}

impl GoalBackedRequestFingerprint {
    pub(crate) fn from_create(
        request: &gents_protocol::request_admission::AgentRequestCreate,
    ) -> Result<Self> {
        // Compile-time fence: adding a new request field requires an explicit
        // decision about whether it belongs in the immutable retry contract.
        let gents_protocol::request_admission::AgentRequestCreate {
            request_id: _,
            purpose: _,
            node_did: _,
            requester_did: _,
            agent_id: _,
            session_id: _,
            retry_parent_request: _,
            retry_parent_request_doc_id: _,
            retry_root_request: _,
            retry_key: _,
            content: _,
            input: _,
            execution_origin: _,
            caused_by_trigger_id: _,
            caused_by_trigger_doc_id: _,
            caused_by_trigger_kind: _,
            caused_by_correlation: _,
            caused_by_trigger_context: _,
            caused_by_source_doc_id: _,
            created_at: _,
            retry_count: _,
            max_retries: _,
            valid_until: _,
            request_hop: _,
            caused_by_parent_request_id: _,
            caused_by_parent_request_doc_id: _,
            caused_by_parent_tool_call_id: _,
            caused_by_parent_tool_call_doc_id: _,
            workspace_id: _,
            workspace_owner_node_did: _,
            workspace_authority: _,
            workspace_seal_hash: _,
            initial_lifecycle_state: _,
            admission: _,
        } = request;
        Ok(Self {
            request_id: request.request_id.clone(),
            purpose: request.purpose,
            node_did: request.node_did.clone(),
            requester_did: request.requester_did.clone(),
            agent_id: request.agent_id.clone(),
            session_id: request.session_id.clone(),
            retry_parent_request: request.retry_parent_request.clone(),
            retry_parent_request_doc_id: request.retry_parent_request_doc_id.clone(),
            retry_root_request: request.retry_root_request.clone(),
            retry_key: request.retry_key.clone(),
            content: request.content.clone(),
            input: request.input.clone(),
            execution_origin: request.execution_origin.clone(),
            caused_by_trigger_id: request.caused_by_trigger_id.clone(),
            caused_by_trigger_doc_id: request.caused_by_trigger_doc_id.clone(),
            caused_by_trigger_kind: request.caused_by_trigger_kind.clone(),
            caused_by_correlation: request.caused_by_correlation.clone(),
            caused_by_trigger_context: request.caused_by_trigger_context.clone(),
            caused_by_source_doc_id: request.caused_by_source_doc_id.clone(),
            retry_count: request.retry_count,
            max_retries: request.max_retries,
            valid_until: request.valid_until.clone(),
            request_hop: i64::from(request.request_hop),
            caused_by_parent_request_id: request.caused_by_parent_request_id.clone(),
            caused_by_parent_request_doc_id: request.caused_by_parent_request_doc_id.clone(),
            caused_by_parent_tool_call_id: request.caused_by_parent_tool_call_id.clone(),
            caused_by_parent_tool_call_doc_id: request.caused_by_parent_tool_call_doc_id.clone(),
            workspace_id: request.workspace_id.clone(),
            workspace_owner_node_did: request.workspace_owner_node_did.clone(),
            workspace_authority: request.workspace_authority.clone(),
            workspace_seal_hash: request.workspace_seal_hash.clone(),
            admission_kind: request.admission.kind.as_str().to_string(),
            admission_signer_did: request.admission.signer_did.clone(),
            enrollment_request_id: request.admission.enrollment_request_id.clone(),
            enrollment_request_digest: request.admission.enrollment_request_digest.clone(),
            enrollment_admin_did: request.admission.enrollment_admin_did.clone(),
            enrollment_authorization_sequence: request
                .admission
                .enrollment_authorization_sequence
                .map(i64::try_from)
                .transpose()
                .context("enrollment authorization sequence exceeds storage range")?,
            enrollment_authorization_expires_at: request
                .admission
                .enrollment_authorization_expires_at
                .clone(),
            runtime_issuer_did: request.admission.runtime_issuer_did.clone(),
            runtime_source_request_id: request.admission.runtime_source_request_id.clone(),
            runtime_source_kind: request
                .admission
                .runtime_source_kind
                .map(|kind| kind.as_str().to_string()),
        })
    }
}
