use gents_protocol::request_admission::{
    validate_signing_fields, AgentRequestAdmissionKind, AgentRequestAdmissionRecord,
    AgentRequestCreate, AgentRequestSigningFields, RequestAdmissionError, RequestPurpose,
    RuntimeInternalSourceKind,
};
use gents_protocol::request_input::RequestInput;
use gents_protocol::row::AgentRequestRow;

use super::IdentityError;

#[derive(Debug, thiserror::Error)]
pub enum RequestReceiptError {
    #[error("{0}")]
    Admission(&'static str),
    #[error(transparent)]
    Canonical(#[from] RequestAdmissionError),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("client request lacks signed physical session or agent identity")]
    ClientTargetsMissing,
    #[error("AgentRequest request_hop must fit in u32")]
    RequestHopOutOfRange(#[source] std::num::TryFromIntError),
    #[error("AgentRequest is missing {0}")]
    MissingField(&'static str),
    #[error("immutable request receipt signature is invalid")]
    InvalidSignature,
    #[error("auxiliary receipt crosses its authenticated parent scope")]
    AuxiliaryScope,
    #[error("runtime local-control receipt does not match expected target/source binding")]
    LocalControlBinding,
    #[error("{0}")]
    AuxiliaryShape(&'static str),
}

type Result<T, E = RequestReceiptError> = std::result::Result<T, E>;

#[cfg(test)]
thread_local! {
    static RECEIPT_VERIFICATION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn count_request_receipt_verifications<T, F>(operation: F) -> (T, usize)
where
    F: FnOnce() -> T,
{
    let before = RECEIPT_VERIFICATION_COUNT.get();
    let value = operation();
    (value, RECEIPT_VERIFICATION_COUNT.get() - before)
}

pub fn verify_request_receipt_signature(row: &AgentRequestRow) -> Result<()> {
    #[cfg(test)]
    RECEIPT_VERIFICATION_COUNT.set(RECEIPT_VERIFICATION_COUNT.get() + 1);
    let admission = row_admission(row)?;
    admission.validate_canonical_fields()?;
    admission
        .validate_branch_fields()
        .map_err(RequestReceiptError::Admission)?;
    let fields = row_signing_fields(row)?;
    validate_signing_fields(&fields)?;
    if !super::verify_did_signature(
        &admission.signer_did,
        &admission.signing_payload(&fields),
        &admission.signature,
    )? {
        return Err(RequestReceiptError::InvalidSignature);
    }
    Ok(())
}

pub(crate) fn signed_request_matches_create(
    row: &AgentRequestRow,
    expected: &AgentRequestCreate,
) -> Result<bool> {
    verify_request_receipt_signature(row)?;
    let admission = row_admission(row)?;
    Ok(admission.signing_payload(&row_signing_fields(row)?) == expected.signing_payload())
}

pub(crate) fn verify_historical_auxiliary_receipt(
    request: &AgentRequestRow,
    parent: &AgentRequestRow,
) -> Result<()> {
    verify_request_receipt_signature(request)?;
    verify_request_receipt_signature(parent)?;
    verify_auxiliary_request_shape(request, &row_admission(request)?)?;
    if !(request.purpose.is_some_and(RequestPurpose::is_auxiliary)
        && parent.purpose == Some(RequestPurpose::Normal)
        && request.doc_id.as_deref().is_some_and(|id| !id.is_empty())
        && parent.doc_id.as_deref().is_some_and(|id| !id.is_empty())
        && request.doc_id != parent.doc_id
        && request.caused_by_parent_request_doc_id == parent.doc_id
        && request.caused_by_parent_request_id.as_deref() == Some(parent.request_id.as_str())
        && request.node_did == parent.node_did
        && request.session_id == parent.session_id
        && request.agent_did == parent.agent_did)
    {
        return Err(RequestReceiptError::AuxiliaryScope);
    }
    Ok(())
}

pub(crate) fn verify_runtime_local_control_receipt(
    row: &AgentRequestRow,
    expected_node_did: &str,
    expected_source_request_id: &str,
    expected_requester_did: &str,
) -> Result<()> {
    let admission = row_admission(row)?;
    verify_request_receipt_signature(row)?;
    if !(row.purpose == Some(RequestPurpose::Normal)
        && admission.kind == AgentRequestAdmissionKind::RuntimeInternal
        && admission.runtime_source_kind == Some(RuntimeInternalSourceKind::LocalControl)
        && row.agent_did.as_deref() == Some(admission.signer_did.as_str())
        && admission.runtime_issuer_did.as_deref() == row.agent_did.as_deref()
        && row.node_did.as_deref() == Some(expected_node_did)
        && row.requester_did.as_deref() == Some(expected_requester_did)
        && admission.runtime_source_request_id.as_deref() == Some(expected_source_request_id)
        && row.caused_by_parent_request_id.as_deref() == Some(expected_source_request_id)
        && row
            .caused_by_parent_request_doc_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
        && row.caused_by_parent_tool_call_id.is_none()
        && row.caused_by_parent_tool_call_doc_id.is_none())
    {
        return Err(RequestReceiptError::LocalControlBinding);
    }
    Ok(())
}

pub(crate) fn row_admission(row: &AgentRequestRow) -> Result<AgentRequestAdmissionRecord> {
    let admission = AgentRequestAdmissionRecord::from_wire_fields(
        row.admission_kind.as_deref(),
        row.admission_signer_did.as_deref(),
        row.admission_signature.as_deref(),
        row.enrollment_request_id.as_deref(),
        row.enrollment_request_digest.as_deref(),
        row.enrollment_admin_did.as_deref(),
        row.enrollment_authorization_sequence,
        row.enrollment_authorization_expires_at.as_deref(),
        row.runtime_issuer_did.as_deref(),
        row.runtime_source_request_id.as_deref(),
        row.runtime_source_kind.as_deref(),
    )
    .map_err(RequestReceiptError::Admission)?;
    if admission.kind == AgentRequestAdmissionKind::Client
        && !(row
            .session_doc_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
            && row.agent_doc_id.as_deref().is_some_and(|id| !id.is_empty()))
    {
        return Err(RequestReceiptError::ClientTargetsMissing);
    }
    Ok(admission)
}

static DEFAULT_REQUEST_INPUT: std::sync::LazyLock<RequestInput> =
    std::sync::LazyLock::new(Default::default);

fn required<'a>(value: Option<&'a str>, field: &'static str) -> Result<&'a str> {
    value.ok_or(RequestReceiptError::MissingField(field))
}

pub(crate) fn row_signing_fields(row: &AgentRequestRow) -> Result<AgentRequestSigningFields<'_>> {
    let request_hop = row
        .request_hop
        .map(u32::try_from)
        .transpose()
        .map_err(RequestReceiptError::RequestHopOutOfRange)?
        .unwrap_or(0);
    Ok(AgentRequestSigningFields {
        request_id: &row.request_id,
        purpose: row
            .purpose
            .ok_or(RequestReceiptError::MissingField("purpose"))?,
        node_did: required(row.node_did.as_deref(), "node_did")?,
        requester_did: row.requester_did.as_deref(),
        agent_did: required(row.agent_did.as_deref(), "agent_did")?,
        session_id: required(row.session_id.as_deref(), "session_id")?,
        session_doc_id: row.session_doc_id.as_deref(),
        agent_doc_id: row.agent_doc_id.as_deref(),
        retry_parent_request: row.retry_parent_request.as_deref(),
        retry_parent_request_doc_id: row.retry_parent_request_doc_id.as_deref(),
        retry_root_request: row.retry_root_request.as_deref(),
        retry_key: row.retry_key.as_deref(),
        content: required(row.content.as_deref(), "content")?,
        input: row.input.as_ref().unwrap_or(&DEFAULT_REQUEST_INPUT),
        execution_origin: row.execution_origin.as_deref(),
        caused_by_trigger_id: row.caused_by_trigger_id.as_deref(),
        caused_by_trigger_kind: row.caused_by_trigger_kind.as_deref(),
        caused_by_correlation: row.caused_by_correlation.as_deref(),
        caused_by_trigger_context: row.caused_by_trigger_context.as_deref(),
        caused_by_source_doc_id: row.caused_by_source_doc_id.as_deref(),
        caused_by_trigger_doc_id: row.caused_by_trigger_doc_id.as_deref(),
        created_at: required(row.created_at.as_deref(), "created_at")?,
        retry_count: row.retry_count,
        max_retries: row.max_retries,
        valid_until: row.valid_until.as_deref(),
        request_hop,
        caused_by_parent_request_id: row.caused_by_parent_request_id.as_deref(),
        caused_by_parent_request_doc_id: row.caused_by_parent_request_doc_id.as_deref(),
        caused_by_parent_tool_call_id: row.caused_by_parent_tool_call_id.as_deref(),
        caused_by_parent_tool_call_doc_id: row.caused_by_parent_tool_call_doc_id.as_deref(),
        workspace_id: row.workspace_id.as_deref(),
        workspace_owner_node_did: row.workspace_owner_node_did.as_deref(),
        workspace_authority: row.workspace_authority.as_deref(),
        workspace_seal_hash: row.workspace_seal_hash.as_deref(),
    })
}

fn shape(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(RequestReceiptError::AuxiliaryShape(message))
    }
}

pub(crate) fn verify_auxiliary_request_shape(
    row: &AgentRequestRow,
    admission: &AgentRequestAdmissionRecord,
) -> Result<()> {
    let target = required(row.agent_did.as_deref(), "agent_did")?;
    let source = required(
        admission.runtime_source_request_id.as_deref(),
        "runtime source request ID",
    )?;
    shape(
        admission.kind == AgentRequestAdmissionKind::RuntimeInternal
            && admission.runtime_source_kind == Some(RuntimeInternalSourceKind::LocalControl)
            && admission.signer_did == target
            && admission.runtime_issuer_did.as_deref() == Some(target)
            && row.requester_did.as_deref() == Some(target),
        "auxiliary request requires target-runtime local-control admission",
    )?;
    shape(
        !source.is_empty()
            && source != row.request_id.as_str()
            && row.caused_by_parent_request_id.as_deref() == Some(source)
            && row.caused_by_parent_request_doc_id.is_some()
            && row.caused_by_parent_tool_call_id.is_none()
            && row.caused_by_parent_tool_call_doc_id.is_none()
            && row.request_hop == Some(0),
        "auxiliary request requires an exact parent-only request link",
    )?;
    shape(
        row.input.as_ref().unwrap_or(&DEFAULT_REQUEST_INPUT) == &*DEFAULT_REQUEST_INPUT
            && row.retry_parent_request.is_none()
            && row.retry_parent_request_doc_id.is_none()
            && row.retry_root_request.as_deref() == Some(row.request_id.as_str())
            && row.retry_key.is_none()
            && row.retry_count == Some(0)
            && row.max_retries == Some(0)
            && row.caused_by_trigger_id.is_none()
            && row.caused_by_trigger_doc_id.is_none()
            && row.caused_by_trigger_kind.is_none()
            && row.caused_by_correlation.is_none()
            && row.caused_by_trigger_context.is_none()
            && row.caused_by_source_doc_id.is_none(),
        "auxiliary request cannot carry input, retry, or trigger lineage",
    )
}
