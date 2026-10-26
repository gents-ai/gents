//! Final, fresh `AgentRequest` authority check. Watcher delivery, replication,
//! queue residence, and caller-authored lineage are not authority.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;
use gents_protocol::request_admission::{
    project_agent_request_admission_disposition, validate_signing_fields,
    AgentRequestAdmissionDisposition, AgentRequestAdmissionKind, AgentRequestAdmissionObservation,
    AgentRequestAdmissionRecord, AgentRequestSigningFields, RequestPurpose,
    RuntimeInternalSourceKind,
};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::agent::p2p_reconcile::{EnrollmentAuthorityHandle, PeerAdmissionAuthority};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::identity::NodeIdentity;
use crate::watcher::AgentRequest;

#[derive(Debug, thiserror::Error)]
pub enum RequestReceiptError {
    /// Admission wire-field or branch rule reported by `gents_protocol`.
    #[error("{reason}")]
    InvalidAdmission { reason: &'static str },
    #[error(transparent)]
    Protocol(#[from] gents_protocol::request_admission::RequestAdmissionError),
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    #[error("AgentRequest request_hop must fit in u32")]
    RequestHop(#[source] std::num::TryFromIntError),
    #[error("AgentRequest is missing {field}")]
    MissingField { field: &'static str },
    #[error("immutable request receipt signature is invalid")]
    InvalidSignature,
    #[error("runtime local-control receipt does not match expected target/source binding")]
    LocalControlBinding,
}

#[derive(Debug)]
pub enum AgentRequestAdmissionError {
    Denied(anyhow::Error),
    Unavailable(anyhow::Error),
}

impl AgentRequestAdmissionError {
    fn denied(error: impl Into<anyhow::Error>) -> Self {
        Self::Denied(error.into())
    }

    fn unavailable(error: impl Into<anyhow::Error>) -> Self {
        Self::Unavailable(error.into())
    }

    pub(crate) const fn is_denied(&self) -> bool {
        matches!(self, Self::Denied(_))
    }
}

impl std::fmt::Display for AgentRequestAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied(error) | Self::Unavailable(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AgentRequestAdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Denied(error) | Self::Unavailable(error) => error.source(),
        }
    }
}

type AdmissionResult<T> = std::result::Result<T, AgentRequestAdmissionError>;

pub fn final_claim_admission_disposition(
    observation_available: bool,
    observation: AgentRequestAdmissionObservation,
) -> AgentRequestAdmissionDisposition {
    project_agent_request_admission_disposition(observation_available, observation)
}

fn base_admission_observation(
    kind: AgentRequestAdmissionKind,
    runtime_source_kind: RuntimeInternalSourceKind,
) -> AgentRequestAdmissionObservation {
    AgentRequestAdmissionObservation {
        kind,
        signature_valid: false,
        signed_fields_match: false,
        branch_fields_exact: false,
        pending_deadline_absent: false,
        signer_matches_requester: false,
        requester_matches_target: false,
        signer_matches_target: false,
        signer_matches_issuer: false,
        requester_matches_session_scope: false,
        current_approval: false,
        exact_generation: false,
        authorization_fresh: false,
        runtime_evidence_present: false,
        runtime_source_kind,
        target_runtime_attestation_valid: false,
        source_binding_current: false,
        trigger_config_document_binding_current: false,
        source_document_binding_current: false,
        target_policy_allows: false,
        peer_authority_allows: false,
        hop_within_bound: false,
    }
}

fn require_admitted_observation(
    observation: AgentRequestAdmissionObservation,
    denied: Option<anyhow::Error>,
) -> AdmissionResult<()> {
    match final_claim_admission_disposition(true, observation) {
        AgentRequestAdmissionDisposition::Admit => Ok(()),
        AgentRequestAdmissionDisposition::Deny => Err(AgentRequestAdmissionError::denied(
            denied.unwrap_or_else(|| anyhow::anyhow!(admission_denial_reason(&observation))),
        )),
        AgentRequestAdmissionDisposition::Retry => Err(AgentRequestAdmissionError::unavailable(
            anyhow::anyhow!("AgentRequest admission observation is unavailable"),
        )),
    }
}

fn admission_denial_reason(observation: &AgentRequestAdmissionObservation) -> &'static str {
    if !observation.pending_deadline_absent {
        "pending AgentRequest carries a caller-authored execution deadline"
    } else if !observation.signed_fields_match {
        "fresh durable AgentRequest does not match the queued request"
    } else if !observation.branch_fields_exact {
        "AgentRequest admission branch fields are invalid"
    } else if !observation.signature_valid {
        "AgentRequest admission signature is invalid"
    } else if !observation.hop_within_bound {
        "AgentRequest causal hop exceeds the target node's max_request_hop"
    } else {
        "fresh AgentRequest admission evidence was denied"
    }
}

fn deny_if(condition: bool, message: &'static str) -> AdmissionResult<()> {
    if condition {
        Ok(())
    } else {
        Err(AgentRequestAdmissionError::denied(anyhow::anyhow!(message)))
    }
}

pub(crate) async fn verify_fresh_local_self_request(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    request: &AgentRequest,
    target_agent_id: &str,
) -> AdmissionResult<AgentRequest> {
    let row = load_signed_request(node, &request.doc_id).await?;
    deny_if(
        row.purpose == Some(RequestPurpose::Normal),
        "local-self claim cannot authorize auxiliary request purpose",
    )?;
    let admission = row_admission(&row).map_err(AgentRequestAdmissionError::denied)?;
    let signing_fields = row_signing_fields(&row).map_err(AgentRequestAdmissionError::denied)?;
    let verified = identity
        .verify(
            &admission.signer_did,
            &admission.signing_payload(&signing_fields),
            &admission.signature,
        )
        .await
        .context("verify local-self AgentRequest signature")
        .map_err(AgentRequestAdmissionError::denied)?;
    let mut observation =
        base_admission_observation(admission.kind, RuntimeInternalSourceKind::LocalControl);
    observation.signature_valid = verified;
    observation.signed_fields_match = row.request_id == request.request_id
        && row.purpose == Some(request.purpose)
        && row.node_did.as_deref() == Some(request.node_did.as_str())
        && row.agent_id.as_deref() == Some(target_agent_id)
        && validate_signing_fields(&signing_fields).is_ok();
    observation.branch_fields_exact =
        admission.validate_canonical_fields().is_ok() && admission.validate_branch_fields().is_ok();
    observation.pending_deadline_absent = row.deadline.is_none();
    observation.signer_matches_requester =
        row.requester_did.as_deref() == Some(admission.signer_did.as_str());
    observation.requester_matches_target = row.requester_did.as_deref() == row.node_did.as_deref();
    observation.hop_within_bound = request_hop_admitted(node, &row).await?;
    require_admitted_observation(observation, None)?;
    verify_request_input(node, &row, &admission).await?;
    row_into_agent_request(row, &request.doc_id).map_err(AgentRequestAdmissionError::denied)
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRequestSigningError {
    #[error(transparent)]
    Admission(#[from] gents_protocol::request_admission::RequestAdmissionError),
    #[error("request admission signer does not match authoring identity")]
    SignerMismatch,
    #[error("sign AgentRequest admission")]
    Sign(#[source] crate::identity::IdentityError),
    #[error("{reason}")]
    BranchFields { reason: &'static str },
    #[error("load registered target runtime identity for AgentRequest authoring")]
    RegisteredIdentity(#[source] crate::identity::IdentityError),
}

pub async fn sign_agent_request_create(
    identity: &dyn NodeIdentity,
    request: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> Result<(), AgentRequestSigningError> {
    validate_signing_fields(&request.signing_fields())?;
    request.admission.validate_canonical_fields()?;
    if request.admission.signer_did != identity.did() {
        return Err(AgentRequestSigningError::SignerMismatch);
    }
    request.admission.signature.clear();
    request.admission.signature = identity
        .sign(&request.signing_payload())
        .await
        .map_err(AgentRequestSigningError::Sign)?;
    request
        .admission
        .validate_branch_fields()
        .map_err(|reason| AgentRequestSigningError::BranchFields { reason })
}

pub(crate) async fn terminalize_pending_request_rejection(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    node_did: &str,
    reason: &str,
    operation: &'static str,
) -> Result<()> {
    let owner = node_did.to_owned();
    let now = Utc::now().to_rfc3339();
    let doc_id = escape_graphql_string(doc_id);
    let node_did = escape_graphql_string(node_did);
    let failure_reason = escape_graphql_string(reason);
    let terminalized_at = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation($terminal_output: JSON) {{
            update_AgentRequest(
                docID: "{doc_id}", filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    lifecycle_state: {{ _eq: "pending" }}
                }},
                input: {{
                    lifecycle_state: "failed",
                    failure_reason: "{failure_reason}",
                    terminalized_at: "{terminalized_at}",
                    terminal_redrive_attempts: 0,
                    terminal_output: $terminal_output
                }}
            ) {{ _docID request_id purpose node_did requester_did agent_id session_id content
                 input created_at caused_by_parent_request_doc_id
                 workspace_id workspace_owner_node_did }}
        }}"#
    );
    let mutation = &mutation;
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node,
        None,
        crate::config_client::IdempotentTransactionRetry::Standard,
        operation,
        |txn| {
            let owner = &owner;
            let now = &now;
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        mutation,
                        &serde_json::json!({
                            "terminal_output": gents_protocol::output::TerminalOutput::NoMessage
                        }),
                    )
                    .await?;
                if let Some(rows) = response
                    .pointer("/data/update_AgentRequest")
                    .and_then(serde_json::Value::as_array)
                {
                    for row in rows {
                        crate::workspace::release_terminal_writer_binding(txn, row).await?;
                        if let Some(request_id) = row["request_id"].as_str() {
                            crate::trigger_engine::durable::publish_request_outcome(
                                txn, owner, request_id, "failed", reason, now,
                            )
                            .await?;
                        }
                        match serde_json::from_value::<AgentRequestRow>(row.clone())
                            .map_err(anyhow::Error::from)
                            .and_then(|row| {
                                crate::watcher::AgentRequest::try_from(row)
                                    .map_err(anyhow::Error::from)
                            })
                        {
                            Ok(request) => {
                                crate::lifecycle::materialize::project_refused_request_session_in_txn(
                                    txn, &request, now,
                                )
                                .await?;
                            }
                            Err(error) => {
                                tracing::warn!(
                                    target: crate::session::REFUSAL_PROJECTION_EVENT_TARGET,
                                    request_id = row["request_id"].as_str().unwrap_or(""),
                                    error = %error,
                                    "refused row skipped session projection: decode failed"
                                );
                            }
                        }
                    }
                }
                Ok(response)
            })
        },
    )
    .await
    .map(|_| ())
}

pub async fn sign_agent_request_create_as_registered_target(
    request: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> Result<(), AgentRequestSigningError> {
    let identity =
        crate::identity::RegisteredIdentity::from_registered_did(request.node_did.clone(), None)
            .map_err(AgentRequestSigningError::RegisteredIdentity)?;
    sign_agent_request_create(&identity, request).await
}

#[derive(Clone)]
pub(crate) struct AgentRequestAdmissionVerifier {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    enrollment: EnrollmentAuthorityHandle,
    peer_admission: Arc<dyn PeerAdmissionAuthority>,
}

impl AgentRequestAdmissionVerifier {
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        identity: Arc<dyn NodeIdentity>,
        enrollment: EnrollmentAuthorityHandle,
    ) -> Self {
        Self {
            node,
            identity,
            enrollment: enrollment.clone(),
            peer_admission: Arc::new(enrollment),
        }
    }

    pub(crate) async fn verify_fresh(
        &self,
        request: &AgentRequest,
        target_agent_id: &str,
    ) -> AdmissionResult<AgentRequest> {
        self.verify_fresh_with_observation(request, target_agent_id, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn verify_fresh_at(
        &self,
        request: &AgentRequest,
        target_agent_id: &str,
        observed_at: chrono::DateTime<Utc>,
    ) -> AdmissionResult<AgentRequest> {
        self.verify_fresh_with_observation(request, target_agent_id, Some(observed_at))
            .await
    }

    async fn verify_fresh_with_observation(
        &self,
        request: &AgentRequest,
        target_agent_id: &str,
        test_observed_at: Option<chrono::DateTime<Utc>>,
    ) -> AdmissionResult<AgentRequest> {
        let row = load_signed_request(self.node.as_ref(), &request.doc_id).await?;
        let admission = row_admission(&row).map_err(AgentRequestAdmissionError::denied)?;
        let signing_fields =
            row_signing_fields(&row).map_err(AgentRequestAdmissionError::denied)?;
        let payload = admission.signing_payload(&signing_fields);
        let signature_valid = self
            .identity
            .verify(&admission.signer_did, &payload, &admission.signature)
            .await
            .context("verify AgentRequest admission signature")
            .map_err(AgentRequestAdmissionError::denied)?;
        let runtime_source_kind = admission
            .runtime_source_kind
            .unwrap_or(RuntimeInternalSourceKind::LocalControl);
        let mut observation = base_admission_observation(admission.kind, runtime_source_kind);
        observation.signature_valid = signature_valid;
        observation.signed_fields_match = row.request_id == request.request_id
            && row.purpose == Some(request.purpose)
            && row.node_did.as_deref() == Some(request.node_did.as_str())
            && row.agent_id.as_deref() == Some(target_agent_id)
            && validate_signing_fields(&signing_fields).is_ok();
        observation.branch_fields_exact = admission.validate_canonical_fields().is_ok()
            && admission.validate_branch_fields().is_ok();
        observation.pending_deadline_absent = row.deadline.is_none();
        observation.hop_within_bound = request_hop_admitted(self.node.as_ref(), &row).await?;
        if !observation.signature_valid
            || !observation.signed_fields_match
            || !observation.branch_fields_exact
            || !observation.pending_deadline_absent
            || !observation.hop_within_bound
        {
            require_admitted_observation(observation, None)?;
            unreachable!("negative common admission evidence cannot be admitted");
        }
        if row.purpose.is_some_and(RequestPurpose::is_auxiliary) {
            verify_auxiliary_request_shape(&row, &admission)?;
        }
        let mut denied = None;
        match admission.kind {
            AgentRequestAdmissionKind::LocalSelf => {
                observation.signer_matches_requester =
                    row.requester_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.node_did.as_deref();
            }
            AgentRequestAdmissionKind::Peer => {
                observation.signer_matches_requester =
                    row.requester_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.node_did.as_deref();
                if observation.signer_matches_requester && !observation.requester_matches_target {
                    observation.peer_authority_allows = self
                        .peer_admission
                        .fresh_member_authorized_for_agent(
                            &admission.signer_did,
                            required_row_string(row.node_did.as_deref(), "node_did")?,
                        )
                        .await
                        .context("reload peer requester admission")
                        .map_err(AgentRequestAdmissionError::unavailable)?;
                    if !observation.peer_authority_allows {
                        denied = Some(anyhow::anyhow!(
                            "peer requester is not authorized for the target node"
                        ));
                    }
                }
            }
            AgentRequestAdmissionKind::Enrollment => {
                let requester = row
                    .requester_did
                    .as_deref()
                    .filter(|did| !did.trim().is_empty());
                observation.signer_matches_requester = requester == Some(&admission.signer_did);
                if !observation.signer_matches_requester {
                    denied = Some(anyhow::anyhow!("enrollment signer is not requester"));
                }
                if let Some(requester) = requester {
                    let current = self
                        .enrollment
                        .fresh_member_authorization(requester)
                        .await
                        .context("fresh enrollment request admission projection")
                        .map_err(AgentRequestAdmissionError::unavailable)?;
                    if let Some(current) = current {
                        let observed_at = test_observed_at.unwrap_or_else(Utc::now);
                        observation.current_approval =
                            row.node_did.as_deref() == Some(current.owner_node.as_str());
                        observation.exact_generation = admission.enrollment_request_id.as_deref()
                            == Some(current.request_id.as_str())
                            && admission.enrollment_request_digest.as_deref()
                                == Some(current.request_digest.as_str())
                            && admission.enrollment_admin_did.as_deref()
                                == Some(current.admin_did.as_str())
                            && admission.enrollment_authorization_sequence
                                == Some(current.authorization_sequence)
                            && admission.enrollment_authorization_expires_at.as_deref()
                                == Some(current.authorization_expires_at.as_str());
                        match chrono::DateTime::parse_from_rfc3339(
                            &current.authorization_expires_at,
                        ) {
                            Ok(expires) => observation.authorization_fresh = observed_at < expires,
                            Err(error) => {
                                denied = Some(
                                    anyhow::Error::new(error)
                                        .context("parse enrollment request authorization expiry"),
                                );
                            }
                        }
                        if !observation.current_approval {
                            denied = Some(anyhow::anyhow!("enrollment target agent mismatch"));
                        } else if !observation.exact_generation {
                            denied = Some(anyhow::anyhow!(
                                "request carries a stale or mixed enrollment generation"
                            ));
                        } else if !observation.authorization_fresh && denied.is_none() {
                            denied =
                                Some(anyhow::anyhow!("enrollment authorization lease expired"));
                        }
                    } else if denied.is_none() {
                        denied = Some(anyhow::anyhow!(
                            "requester has no current enrollment authorization"
                        ));
                    }
                } else if denied.is_none() {
                    denied = Some(anyhow::anyhow!("enrollment request has no requester DID"));
                }
            }
            AgentRequestAdmissionKind::RuntimeInternal => {
                let issuer = admission.runtime_issuer_did.as_deref();
                let source = admission.runtime_source_request_id.as_deref();
                observation.runtime_evidence_present =
                    issuer.is_some() && source.is_some() && admission.runtime_source_kind.is_some();
                observation.signer_matches_issuer = issuer == Some(&admission.signer_did);
                observation.signer_matches_target =
                    row.node_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.node_did.as_deref();
                observation.target_runtime_attestation_valid = issuer == row.node_did.as_deref();
                let target = required_row_string(row.node_did.as_deref(), "node_did")?;
                let session_scope = if row.purpose.is_some_and(RequestPurpose::is_auxiliary) {
                    None
                } else {
                    crate::session::load_session_requester_scope(
                        self.node.as_ref(),
                        target,
                        required_row_string(row.session_id.as_deref(), "session_id")?,
                    )
                    .await
                    .map_err(AgentRequestAdmissionError::unavailable)?
                };
                observation.requester_matches_session_scope = row.requester_did.as_deref()
                    == Some(session_scope.as_deref().unwrap_or(target));
                if !observation.runtime_evidence_present
                    || !observation.signer_matches_issuer
                    || !observation.signer_matches_target
                    || !observation.requester_matches_session_scope
                    || !observation.target_runtime_attestation_valid
                {
                    require_admitted_observation(observation, None)?;
                    unreachable!("invalid runtime attestation cannot be admitted");
                }
                if let (Some(source), Some(source_kind)) = (source, admission.runtime_source_kind) {
                    match verify_runtime_source_binding(
                        self.node.clone(),
                        &row,
                        row.purpose
                            .context("AgentRequest is missing purpose")
                            .map_err(AgentRequestAdmissionError::denied)?,
                        source,
                        source_kind,
                        target_agent_id,
                    )
                    .await
                    {
                        Ok(()) => {
                            observation.source_binding_current = true;
                            match source_kind {
                                RuntimeInternalSourceKind::LocalControl => {
                                    observation.source_document_binding_current = true;
                                }
                                RuntimeInternalSourceKind::AutomatedTrigger => {
                                    observation.trigger_config_document_binding_current = true;
                                    observation.target_policy_allows = true;
                                }
                            }
                        }
                        Err(AgentRequestAdmissionError::Denied(error)) => denied = Some(error),
                        Err(error @ AgentRequestAdmissionError::Unavailable(_)) => {
                            return Err(error);
                        }
                    }
                }
            }
        }
        require_admitted_observation(observation, denied)?;
        verify_request_input(&self.node, &row, &admission).await?;
        row_into_agent_request(row, &request.doc_id).map_err(AgentRequestAdmissionError::denied)
    }
}

fn verify_auxiliary_request_shape(
    row: &AgentRequestRow,
    admission: &AgentRequestAdmissionRecord,
) -> AdmissionResult<()> {
    let target = required_row_string(row.node_did.as_deref(), "node_did")?;
    let source = required_row_string(
        admission.runtime_source_request_id.as_deref(),
        "runtime source request ID",
    )?;
    deny_if(
        admission.kind == AgentRequestAdmissionKind::RuntimeInternal
            && admission.runtime_source_kind == Some(RuntimeInternalSourceKind::LocalControl)
            && admission.signer_did == target
            && admission.runtime_issuer_did.as_deref() == Some(target)
            && row.requester_did.as_deref() == Some(target),
        "auxiliary request requires target-runtime local-control admission",
    )?;
    deny_if(
        !source.is_empty()
            && source != row.request_id.as_str()
            && row.caused_by_parent_request_id.as_deref() == Some(source)
            && row.caused_by_parent_request_doc_id.is_some()
            && row.caused_by_parent_tool_call_id.is_none()
            && row.caused_by_parent_tool_call_doc_id.is_none()
            && row.request_hop == Some(0),
        "auxiliary request requires an exact parent-only request link",
    )?;
    deny_if(
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

async fn verify_request_input(
    node: &Arc<EmbeddedNode>,
    row: &AgentRequestRow,
    admission: &AgentRequestAdmissionRecord,
) -> AdmissionResult<()> {
    use gents_protocol::request_input::QueueSource;
    let input = row.input.as_ref().unwrap_or(&DEFAULT_REQUEST_INPUT);
    if row.purpose.is_some_and(RequestPurpose::is_auxiliary) {
        return deny_if(
            input == &*DEFAULT_REQUEST_INPUT,
            "auxiliary request cannot carry ordinary request input",
        );
    }
    deny_if(
        input.captured_context.iter().all(|context| context.is_well_formed()),
        "captured context requires an ordered UTF-16 range and a positive buffer version when present",
    )?;
    input
        .validate_attachments()
        .map_err(AgentRequestAdmissionError::denied)?;
    let owner = required_row_string(row.node_did.as_deref(), "node_did")?;
    let agent = required_row_string(row.agent_id.as_deref(), "agent_id")?;
    let (context, tools, _) = load_request_context(node, owner, agent).await?;
    let skills = context
        .as_ref()
        .map(|context| context.skill_ids.as_slice())
        .unwrap_or_default();
    deny_if(
        input
            .selected_skill_ids
            .iter()
            .all(|id| skills.contains(id)),
        "request activates a skill outside its context allowlist",
    )?;
    let request =
        AgentRequest::try_from(row.clone()).map_err(AgentRequestAdmissionError::denied)?;
    let artifact_requested = tools
        .as_ref()
        .and_then(|tools| tools.host.as_ref())
        .and_then(|host| host.bash.as_ref())
        .and_then(|bash| bash.execution_mode)
        == Some(crate::toolset::CommandExecutionMode::ArtifactWrite);
    crate::workspace::validate_request_workspace_input(node, &request, artifact_requested)
        .await
        .map_err(AgentRequestAdmissionError::denied)?;
    let runtime_control = admission.kind == AgentRequestAdmissionKind::RuntimeInternal
        && admission.runtime_source_kind == Some(RuntimeInternalSourceKind::LocalControl);
    if let Some(queue) = &input.queue {
        let session_message_steering = queue.source == QueueSource::Steering
            && row.caused_by_parent_request_doc_id.is_some()
            && row.caused_by_parent_tool_call_doc_id.is_some();
        deny_if(
            queue.source == QueueSource::User || runtime_control || session_message_steering,
            "runtime queue source requires authenticated local-control issuance",
        )?;
        if queue.position.is_some() {
            crate::config_client::ConfigAccess::transact_local_readonly(
                node,
                None,
                "admission.queue_position",
                |txn| {
                    Box::pin(
                        async move { crate::lifecycle::queue::validate_position(txn, row).await },
                    )
                },
            )
            .await
            .map_err(|error| {
                if matches!(
                    error,
                    crate::lifecycle::queue::QueueError::InvalidQueuePosition(_)
                ) {
                    AgentRequestAdmissionError::denied(error)
                } else {
                    AgentRequestAdmissionError::unavailable(error)
                }
            })?;
        }
    }
    if input.goal_continuation.is_some()
        || input
            .queue
            .as_ref()
            .is_some_and(|queue| queue.source == QueueSource::Goal)
    {
        deny_if(
            runtime_control && input.goal_continuation.is_some(),
            "goal continuation requires authenticated original issuance facts",
        )?;
        let parent = load_exact_parent_request(
            node,
            required_row_string(
                row.caused_by_parent_request_doc_id.as_deref(),
                "parent document ID",
            )?,
        )
        .await?;
        crate::goal::verify_goal_continuation_edge(
            owner,
            required_row_string(row.session_id.as_deref(), "session_id")?,
            required_row_string(row.caused_by_trigger_id.as_deref(), "goal_id")?,
            &parent,
            row,
        )
        .map_err(AgentRequestAdmissionError::denied)?;
    }
    Ok(())
}

async fn request_hop_admitted(node: &EmbeddedNode, row: &AgentRequestRow) -> AdmissionResult<bool> {
    let hop = row.request_hop.unwrap_or(0);
    let Ok(hop) = u32::try_from(hop) else {
        return Ok(false);
    };
    if hop == 0 {
        return Ok(true);
    }
    let target = required_row_string(row.node_did.as_deref(), "node_did")?;
    let max_request_hop = max_request_hop(node, target)
        .await
        .map_err(AgentRequestAdmissionError::unavailable)?;
    Ok(crate::lifecycle::request_hop_within_bound(
        max_request_hop,
        hop,
    ))
}

pub(crate) async fn max_request_hop(
    node: &EmbeddedNode,
    node_did: &str,
) -> anyhow::Result<Option<u32>> {
    Ok(crate::document_config::load_node(node, node_did)
        .await?
        .and_then(|node_doc| node_doc.max_request_hop))
}

fn request_workspace(row: &AgentRequestRow) -> crate::lifecycle::WorkspaceLineage {
    crate::lifecycle::WorkspaceLineage {
        workspace_id: row.workspace_id.clone(),
        workspace_owner_node_did: row.workspace_owner_node_did.clone(),
        workspace_authority: row.workspace_authority.clone(),
        workspace_seal_hash: row.workspace_seal_hash.clone(),
    }
}

async fn verify_runtime_source_binding(
    node: Arc<EmbeddedNode>,
    row: &AgentRequestRow,
    purpose: RequestPurpose,
    source: &str,
    source_kind: RuntimeInternalSourceKind,
    target_agent_id: &str,
) -> AdmissionResult<()> {
    match source_kind {
        RuntimeInternalSourceKind::LocalControl => {
            deny_if(
                row.caused_by_parent_request_id.as_deref() == Some(source)
                    && row.caused_by_parent_tool_call_id.is_none()
                    && row.caused_by_parent_tool_call_doc_id.is_none(),
                "local-control runtime source branch is mixed or incoherent",
            )?;
            let parent_doc_id =
                row.caused_by_parent_request_doc_id
                    .as_deref()
                    .ok_or_else(|| {
                        AgentRequestAdmissionError::denied(anyhow::anyhow!(
                            "local-control parent document binding is absent"
                        ))
                    })?;
            let parent = load_exact_parent_request(node.as_ref(), parent_doc_id).await?;
            deny_if(
                parent.request_id == source && parent.node_did == row.node_did,
                "local-control parent document does not exactly own the source",
            )?;
            if purpose.is_auxiliary() {
                return deny_if(
                    parent.session_id == row.session_id
                        && parent.agent_id.as_deref() == Some(target_agent_id),
                    "auxiliary request parent does not match its session and agent",
                );
            }
            deny_if(
                parent.session_id == row.session_id
                    && parent
                        .requester_did
                        .as_deref()
                        .or(parent.node_did.as_deref())
                        == row.requester_did.as_deref(),
                "local-control parent is outside its session or requester scope",
            )?;
            request_workspace(row)
                .validate_source(&request_workspace(&parent), true)
                .map_err(AgentRequestAdmissionError::denied)
        }
        RuntimeInternalSourceKind::AutomatedTrigger => {
            deny_if(
                row.caused_by_trigger_id.as_deref() == Some(source)
                    && matches!(
                        row.caused_by_trigger_kind.as_deref(),
                        Some("event" | "schedule")
                    ),
                "automated-trigger runtime source branch is mixed or incoherent",
            )?;
            verify_automated_trigger_source(
                node.as_ref(),
                row.caused_by_trigger_kind.as_deref().unwrap_or_default(),
                source,
                row.caused_by_trigger_doc_id.as_deref(),
                target_agent_id,
                required_row_string(row.node_did.as_deref(), "node_did")?,
            )
            .await
        }
    }
}

async fn load_exact_parent_request(
    node: &EmbeddedNode,
    source_doc_id: &str,
) -> AdmissionResult<AgentRequestRow> {
    let row = load_signed_request(node, source_doc_id).await?;
    deny_if(
        row.doc_id.as_deref() == Some(source_doc_id),
        "runtime source parent physical document binding changed",
    )?;
    Ok(row)
}

pub(crate) async fn load_request_context(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) -> AdmissionResult<(
    Option<crate::document_config::AgentContext>,
    Option<crate::document_config::Tools>,
    Vec<crate::document_config::AgentTargetDocument>,
)> {
    use crate::config_client::ConfigAccess;
    let owner = node_did.to_owned();
    let agent_id = agent_id.to_owned();
    ConfigAccess::transact_local_readonly(node, None, "request_admission.context", move |txn| {
        let owner = owner.clone();
        let agent_id = agent_id.clone();
        Box::pin(async move { load_request_context_in_txn(txn, &owner, &agent_id).await })
    })
    .await
    .map_err(
        |error| match error.downcast::<AgentRequestAdmissionError>() {
            Ok(error) => error,
            Err(error) => AgentRequestAdmissionError::unavailable(error),
        },
    )
}

/// [`load_request_context`] inside `txn`; a refusal is an
/// [`AgentRequestAdmissionError`] in the returned error.
pub(crate) async fn load_request_context_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    agent_id: &str,
) -> anyhow::Result<(
    Option<crate::document_config::AgentContext>,
    Option<crate::document_config::Tools>,
    Vec<crate::document_config::AgentTargetDocument>,
)> {
    use crate::collection::Collection;
    use crate::config_client::read_desired_state_document_in_txn as read;
    use crate::document_config::{Agent, AgentContext, AgentTargetDocument, Tools};
    let agent: Agent = serde_json::from_value(
        read(txn, Collection::Agent, owner, agent_id)
            .await?
            .ok_or_else(|| {
                AgentRequestAdmissionError::denied(anyhow::anyhow!("request agent is missing"))
            })?,
    )?;
    deny_if(agent.enabled, "request agent is disabled")?;
    let Some(context_id) = agent.context_id else {
        return Ok((None, None, Vec::new()));
    };
    let context: AgentContext = serde_json::from_value(
        read(txn, Collection::AgentContext, owner, &context_id)
            .await?
            .ok_or_else(|| {
                AgentRequestAdmissionError::denied(anyhow::anyhow!("request context is missing"))
            })?,
    )?;
    let tools: Option<Tools> = match context.tools_id.as_deref() {
        None => None,
        Some(id) => Some(serde_json::from_value(
            read(txn, Collection::Tools, owner, id)
                .await?
                .ok_or_else(|| {
                    AgentRequestAdmissionError::denied(anyhow::anyhow!("request tools are missing"))
                })?,
        )?),
    };
    let mut targets = Vec::<AgentTargetDocument>::new();
    if let Some(agents) = tools.as_ref().and_then(|tools| tools.agents.as_ref()) {
        for id in &agents.target_ids {
            targets.push(serde_json::from_value(
                read(txn, Collection::AgentTarget, owner, id)
                    .await?
                    .ok_or_else(|| {
                        AgentRequestAdmissionError::denied(anyhow::anyhow!(
                            "request agent target is missing"
                        ))
                    })?,
            )?);
        }
    }
    Ok((Some(context), tools, targets))
}

async fn verify_automated_trigger_source(
    node: &EmbeddedNode,
    kind: &str,
    trigger_id: &str,
    trigger_doc_id: Option<&str>,
    target_agent_id: &str,
    node_did: &str,
) -> AdmissionResult<()> {
    #[derive(Deserialize)]
    struct TriggerRow {
        trigger_id: String,
        node_did: String,
        task_id: String,
        source: crate::document_config::TriggerSource,
        enabled: bool,
    }
    #[derive(Deserialize)]
    struct TaskRow {
        agent_id: String,
        enabled: bool,
    }
    let doc = required_row_string(trigger_doc_id, "trigger document ID")?;
    let response = graphql_with_transaction_retry(node, &format!(
        r#"{{ Trigger(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ trigger_id node_did task_id source enabled }} }}"#,
        escape_graphql_string(doc),
    ), "reload runtime trigger").await.map_err(AgentRequestAdmissionError::unavailable)?;
    let triggers: Vec<TriggerRow> =
        crate::graphql::rows(&response, "Trigger").map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        triggers.len() == 1,
        "runtime trigger is missing or ambiguous",
    )?;
    let trigger = &triggers[0];
    deny_if(
        trigger.enabled
            && trigger.trigger_id == trigger_id
            && trigger.node_did == node_did
            && matches!(
                (&trigger.source, kind),
                (crate::document_config::TriggerSource::Event { .. }, "event")
                    | (
                        crate::document_config::TriggerSource::Schedule { .. },
                        "schedule"
                    )
            ),
        "runtime trigger physical source, node, kind, or availability changed",
    )?;
    let response = graphql_with_transaction_retry(node, &format!(
        r#"{{ Task(filter: {{ task_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }} }}, limit: 2) {{ agent_id enabled }} }}"#,
        escape_graphql_string(&trigger.task_id), escape_graphql_string(node_did),
    ), "reload runtime trigger task").await.map_err(AgentRequestAdmissionError::unavailable)?;
    let tasks: Vec<TaskRow> =
        crate::graphql::rows(&response, "Task").map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        tasks.len() == 1 && tasks[0].enabled && tasks[0].agent_id == target_agent_id,
        "runtime trigger task is missing, disabled, ambiguous, or targets another agent",
    )
}

pub fn verify_request_receipt_signature(row: &AgentRequestRow) -> Result<(), RequestReceiptError> {
    let admission = row_admission(row)?;
    admission.validate_canonical_fields()?;
    admission
        .validate_branch_fields()
        .map_err(|reason| RequestReceiptError::InvalidAdmission { reason })?;
    let fields = row_signing_fields(row)?;
    validate_signing_fields(&fields)?;
    if !crate::identity::verify_did_signature(
        &admission.signer_did,
        &admission.signing_payload(&fields),
        &admission.signature,
    )? {
        return Err(RequestReceiptError::InvalidSignature);
    }
    Ok(())
}

pub(crate) fn verify_historical_auxiliary_receipt(
    request: &AgentRequestRow,
    parent: &AgentRequestRow,
) -> Result<()> {
    verify_request_receipt_signature(request)?;
    verify_request_receipt_signature(parent)?;
    verify_auxiliary_request_shape(request, &row_admission(request)?)?;
    anyhow::ensure!(
        request.purpose.is_some_and(RequestPurpose::is_auxiliary)
            && parent.purpose == Some(RequestPurpose::Normal)
            && request.doc_id.as_deref().is_some_and(|id| !id.is_empty())
            && parent.doc_id.as_deref().is_some_and(|id| !id.is_empty())
            && request.doc_id != parent.doc_id
            && request.caused_by_parent_request_doc_id == parent.doc_id
            && request.caused_by_parent_request_id.as_deref() == Some(parent.request_id.as_str())
            && request.node_did == parent.node_did
            && request.session_id == parent.session_id
            && request.agent_id == parent.agent_id,
        "auxiliary receipt crosses its authenticated parent scope"
    );
    Ok(())
}

pub(crate) fn verify_runtime_local_control_receipt(
    row: &AgentRequestRow,
    expected_target_did: &str,
    expected_source_request_id: &str,
    expected_requester_did: &str,
) -> Result<(), RequestReceiptError> {
    let admission = row_admission(row)?;
    verify_request_receipt_signature(row)?;
    if !(row.purpose == Some(RequestPurpose::Normal)
        && admission.kind == AgentRequestAdmissionKind::RuntimeInternal
        && admission.runtime_source_kind == Some(RuntimeInternalSourceKind::LocalControl)
        && admission.signer_did == expected_target_did
        && admission.runtime_issuer_did.as_deref() == Some(expected_target_did)
        && row.node_did.as_deref() == Some(expected_target_did)
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

fn row_admission(
    row: &AgentRequestRow,
) -> Result<AgentRequestAdmissionRecord, RequestReceiptError> {
    AgentRequestAdmissionRecord::from_wire_fields(
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
    .map_err(|reason| RequestReceiptError::InvalidAdmission { reason })
}

static DEFAULT_REQUEST_INPUT: std::sync::LazyLock<gents_protocol::request_input::RequestInput> =
    std::sync::LazyLock::new(Default::default);

fn row_signing_fields(
    row: &AgentRequestRow,
) -> Result<AgentRequestSigningFields<'_>, RequestReceiptError> {
    let missing = |field| RequestReceiptError::MissingField { field };
    let request_hop = row
        .request_hop
        .map(u32::try_from)
        .transpose()
        .map_err(RequestReceiptError::RequestHop)?
        .unwrap_or(0);
    Ok(AgentRequestSigningFields {
        request_id: &row.request_id,
        purpose: row.purpose.ok_or_else(|| missing("purpose"))?,
        node_did: row.node_did.as_deref().ok_or_else(|| missing("node_did"))?,
        requester_did: row.requester_did.as_deref(),
        agent_id: row.agent_id.as_deref().ok_or_else(|| missing("agent_id"))?,
        session_id: row
            .session_id
            .as_deref()
            .ok_or_else(|| missing("session_id"))?,
        retry_parent_request: row.retry_parent_request.as_deref(),
        retry_parent_request_doc_id: row.retry_parent_request_doc_id.as_deref(),
        retry_root_request: row.retry_root_request.as_deref(),
        retry_key: row.retry_key.as_deref(),
        content: row.content.as_deref().ok_or_else(|| missing("content"))?,
        input: row.input.as_ref().unwrap_or(&DEFAULT_REQUEST_INPUT),
        execution_origin: row.execution_origin.as_deref(),
        caused_by_trigger_id: row.caused_by_trigger_id.as_deref(),
        caused_by_trigger_kind: row.caused_by_trigger_kind.as_deref(),
        caused_by_correlation: row.caused_by_correlation.as_deref(),
        caused_by_trigger_context: row.caused_by_trigger_context.as_deref(),
        caused_by_source_doc_id: row.caused_by_source_doc_id.as_deref(),
        caused_by_trigger_doc_id: row.caused_by_trigger_doc_id.as_deref(),
        created_at: row
            .created_at
            .as_deref()
            .ok_or_else(|| missing("created_at"))?,
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

fn row_into_agent_request(row: AgentRequestRow, expected_doc_id: &str) -> Result<AgentRequest> {
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(expected_doc_id),
        "fresh AgentRequest document binding changed before admission"
    );
    Ok(AgentRequest::try_from(row)?)
}

fn required_row_string<'a>(value: Option<&'a str>, field: &str) -> AdmissionResult<&'a str> {
    value.ok_or_else(|| {
        AgentRequestAdmissionError::denied(anyhow::anyhow!("AgentRequest is missing {field}"))
    })
}

macro_rules! signed_request_receipt_fields {
    () => {
        r#"
_docID request_id purpose node_did requester_did agent_id session_id
                retry_parent_request retry_parent_request_doc_id retry_root_request retry_key
                content input max_total_tokens
                execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_correlation
                caused_by_trigger_context caused_by_source_doc_id caused_by_trigger_doc_id
                created_at deadline execution_generation execution_lease_expires_at retry_count
                max_retries valid_until request_hop caused_by_parent_request_id
                caused_by_parent_request_doc_id caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id workspace_id workspace_owner_node_did workspace_authority
                workspace_seal_hash admission_kind
                admission_signer_did admission_signature enrollment_request_id
                enrollment_request_digest enrollment_admin_did enrollment_authorization_sequence
                enrollment_authorization_expires_at runtime_issuer_did runtime_source_request_id
                runtime_source_kind
"#
    };
}

/// Historical receipt verification does not depend on mutable lifecycle state.
pub(crate) const SIGNED_REQUEST_RECEIPT_FIELDS: &str = signed_request_receipt_fields!();

/// Lifecycle is included for callers decoding the row, but is not part of the
/// signed payload.
pub const SIGNED_REQUEST_FIELDS: &str =
    concat!("lifecycle_state ", signed_request_receipt_fields!());

async fn load_signed_request(
    node: &EmbeddedNode,
    doc_id: &str,
) -> AdmissionResult<AgentRequestRow> {
    let doc_id = escape_graphql_string(doc_id);
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                {SIGNED_REQUEST_FIELDS}
            }} }}"#,
        ),
        "reload AgentRequest admission row",
    )
    .await
    .map_err(AgentRequestAdmissionError::unavailable)?;
    crate::graphql::first_row(&response, "AgentRequest")
        .map_err(AgentRequestAdmissionError::denied)?
        .ok_or_else(|| {
            AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
                "AgentRequest disappeared before admission"
            ))
        })
}

#[cfg(test)]
pub(crate) async fn load_request_for_admission_test(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<AgentRequest> {
    let row = load_signed_request(node, doc_id)
        .await
        .map_err(anyhow::Error::from)?;
    row_into_agent_request(row, doc_id)
}

#[cfg(test)]
mod title_tests;

#[cfg(test)]
mod captured_context_tests;

#[cfg(test)]
pub(crate) mod workspace_cleanup_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::AgentRequestAdmissionVerifier;
    use crate::agent::p2p_reconcile::enrollment_authority_channel;
    use crate::identity::{KeyIdentity, NodeIdentity};
    use crate::schema::ensure_runtime_schemas;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

    #[tokio::test]
    async fn pending_admission_rejection_selects_terminal_no_message() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(temp.path().join("rejection.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "rejected-request",
            identity.did(),
            identity.did(),
            "agent",
            "session",
            "work",
            "interactive",
            "2026-09-01T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let created = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        let doc_id = crate::graphql::single_mutation_document(&created, "create_AgentRequest")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();

        super::terminalize_pending_request_rejection(
            &node,
            &doc_id,
            identity.did(),
            "request activates a skill outside its context allowlist",
            "test.admission_rejection",
        )
        .await
        .unwrap();

        let escaped_doc_id = crate::graphql::escape_graphql_string(&doc_id);
        let selected = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{ lifecycle_state failure_reason terminal_output }} }}"#
            ))
            .await;
        assert!(!selected.has_errors(), "{:?}", selected.errors);
        let row = crate::graphql::first_row::<serde_json::Value>(&selected, "AgentRequest")
            .unwrap()
            .unwrap();
        assert_eq!(row["lifecycle_state"], "failed");
        assert_eq!(
            row["failure_reason"],
            "request activates a skill outside its context allowlist"
        );
        let terminal_output: gents_protocol::output::TerminalOutput =
            serde_json::from_value(row["terminal_output"].clone()).unwrap();
        assert_eq!(
            terminal_output,
            gents_protocol::output::TerminalOutput::NoMessage
        );
    }

    #[tokio::test]
    async fn signed_input_cannot_expand_context_or_spoof_runtime_queue() {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("input.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let owner = crate::graphql::escape_graphql_string(identity.did());
        for mutation in [
            format!(
                r#"mutation {{ create_AgentContext(input: {{ context_id: "context", node_did: "{owner}", skill_ids: ["allowed"] }}) {{ _docID }} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{ agent_id: "agent", node_did: "{owner}", context_id: "context", inference_profile_id: "inference", enabled: true }}) {{ _docID }} }}"#
            ),
        ] {
            let result = node.execute(&mutation).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
        }
        for (index, input, allowed) in [
            (
                0,
                serde_json::json!({"selected_skill_ids":["allowed"]}),
                true,
            ),
            (
                1,
                serde_json::json!({"selected_skill_ids":["unconfigured"]}),
                false,
            ),
            (
                2,
                serde_json::json!({"queue":{"source":"steering","policy":"append"}}),
                false,
            ),
            (
                3,
                serde_json::json!({"queue":{"source":"goal","policy":"coalesce"},"goal_continuation":{"sequence":1,"wrapup":false}}),
                false,
            ),
        ] {
            let mut create = AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                format!("input-{index}"),
                identity.did(),
                identity.did(),
                "agent",
                "session",
                "work",
                "interactive",
                "2026-09-01T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(identity.did()),
            );
            create.input = serde_json::from_value(input).unwrap();
            crate::sign_agent_request_create(&identity, &mut create)
                .await
                .unwrap();
            let result = node.execute(&create.graphql_mutation().unwrap()).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
            let doc = crate::graphql::single_mutation_document(&result, "create_AgentRequest")
                .unwrap()
                .unwrap()["_docID"]
                .as_str()
                .unwrap()
                .to_owned();
            let request = super::load_request_for_admission_test(&node, &doc)
                .await
                .unwrap();
            let admitted =
                super::verify_fresh_local_self_request(&node, &identity, &request, "agent").await;
            assert_eq!(admitted.is_ok(), allowed, "case {index}: {admitted:?}");
        }
    }

    #[tokio::test]
    async fn admission_rejection_projects_the_failed_turn_into_the_request_owned_session_only() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(temp.path().join("projection.key"), None).unwrap();
        let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let owner = crate::graphql::escape_graphql_string(identity.did());
        for mutation in [
            format!(
                r#"mutation {{ create_AgentContext(input: {{ context_id: "context", node_did: "{owner}", skill_ids: ["allowed"] }}) {{ _docID }} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{ agent_id: "agent", node_did: "{owner}", context_id: "context", inference_profile_id: "inference", enabled: true }}) {{ _docID }} }}"#
            ),
        ] {
            let result = node.execute(&mutation).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
        }
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &node,
            "foreign-session",
            identity.did(),
            "agent",
            Some(foreign.did()),
        )
        .await
        .unwrap();

        let refusal_reason =
            "request admission denied: request activates a skill outside its context allowlist";
        for (request_id, session_id) in [
            ("refused-skill", "session"),
            ("foreign-scope", "foreign-session"),
        ] {
            let mut create = AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                request_id,
                identity.did(),
                identity.did(),
                "agent",
                session_id,
                "run the disallowed skill",
                "interactive",
                "2026-09-01T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(identity.did()),
            );
            create.input =
                serde_json::from_value(serde_json::json!({"selected_skill_ids": ["unconfigured"]}))
                    .unwrap();
            crate::sign_agent_request_create(&identity, &mut create)
                .await
                .unwrap();
            let result = node.execute(&create.graphql_mutation().unwrap()).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
            let doc_id = crate::graphql::single_mutation_document(&result, "create_AgentRequest")
                .unwrap()
                .unwrap()["_docID"]
                .as_str()
                .unwrap()
                .to_owned();

            let request = super::load_request_for_admission_test(&node, &doc_id)
                .await
                .unwrap();
            assert!(
                super::verify_fresh_local_self_request(&node, &identity, &request, "agent")
                    .await
                    .is_err(),
                "{request_id} must be denied by the context allowlist"
            );

            super::terminalize_pending_request_rejection(
                &node,
                &doc_id,
                identity.did(),
                refusal_reason,
                "test.admission_rejection_projection",
            )
            .await
            .unwrap();

            let selected = node
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{owner}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::graphql::escape_graphql_string(session_id),
                    crate::session::AGENT_SESSION_FIELDS,
                ))
                .await;
            assert!(!selected.has_errors(), "{:?}", selected.errors);
            let rows = selected.data.as_ref().unwrap()["AgentSession"]
                .as_array()
                .cloned()
                .unwrap();
            assert_eq!(rows.len(), 1, "{request_id} must not add session rows");
            let session = crate::session::decode_session_row(&rows[0])
                .unwrap()
                .session;
            if session_id == "session" {
                assert_eq!(session.requester_did.as_deref(), Some(identity.did()));
                let latest = session
                    .observation
                    .expect("refused request advances the session observation")
                    .latest_request
                    .expect("observation names the refused request");
                assert_eq!(latest.request_id, request_id);
                assert_eq!(
                    latest.lifecycle_state,
                    gents_protocol::request_lifecycle::RequestLifecycleState::Failed
                );
            } else {
                assert!(
                    session.observation.is_none(),
                    "a refusal must not touch a session owned by another requester"
                );
                assert_eq!(session.requester_did.as_deref(), Some(foreign.did()));
            }
        }
        node.shutdown().await;
    }

    async fn refused_pending_request_under_session(
        node: &defra_node::EmbeddedNode,
        identity: &crate::identity::KeyIdentity,
        request_id: &str,
        session_id: &str,
    ) -> String {
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            "agent",
            session_id,
            "run the disallowed skill",
            "interactive",
            "2026-09-01T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        create.input =
            serde_json::from_value(serde_json::json!({"selected_skill_ids": ["unconfigured"]}))
                .unwrap();
        crate::sign_agent_request_create(identity, &mut create)
            .await
            .unwrap();
        let result = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        crate::graphql::single_mutation_document(&result, "create_AgentRequest")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn refusal_survives_undecodable_session_rows_sharing_its_label() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(temp.path().join("corrupt-session.key"), None).unwrap();
        let foreign =
            KeyIdentity::load_or_create(temp.path().join("corrupt-foreign.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let node_did = crate::graphql::escape_graphql_string(identity.did());
        for (session_id, requester_did) in [
            ("corrupt-foreign-scope", Some(foreign.did())),
            ("corrupt-own-scope", Some(identity.did())),
        ] {
            let requester = match requester_did {
                Some(did) => format!(
                    r#"requester_did: "{}","#,
                    crate::graphql::escape_graphql_string(did)
                ),
                None => "requester_did: null,".to_owned(),
            };
            let seeded = node
                .execute(&format!(
                    r#"mutation {{ create_AgentSession(input: {{
                        session_id: "{}", node_did: "{node_did}", {requester} agent_id: "agent",
                        created_at: "not-an-rfc3339-timestamp"
                    }}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(session_id),
                ))
                .await;
            assert!(!seeded.has_errors(), "{session_id}: {:?}", seeded.errors);

            let request_id = format!("refused-under-{session_id}");
            let doc_id =
                refused_pending_request_under_session(&node, &identity, &request_id, session_id)
                    .await;
            super::terminalize_pending_request_rejection(
                &node,
                &doc_id,
                identity.did(),
                "request admission denied: request activates a skill outside its context allowlist",
                "test.admission_rejection_corrupt_session_row",
            )
            .await
            .unwrap_or_else(|error| panic!("{session_id}: the refusal must persist: {error}"));

            let selected = node
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                    crate::graphql::escape_graphql_string(&doc_id),
                ))
                .await;
            assert!(!selected.has_errors(), "{:?}", selected.errors);
            assert_eq!(
                selected.data.as_ref().unwrap()["AgentRequest"][0]["lifecycle_state"],
                "failed",
                "{session_id}: the refused request must not stay pending"
            );
            let sessions = node
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::graphql::escape_graphql_string(session_id),
                    crate::session::AGENT_SESSION_FIELDS,
                ))
                .await;
            assert!(!sessions.has_errors(), "{:?}", sessions.errors);
            let rows = sessions.data.as_ref().unwrap()["AgentSession"]
                .as_array()
                .cloned()
                .unwrap();
            assert_eq!(
                rows.len(),
                1,
                "{session_id}: the refusal must not mint beside an undecodable row"
            );
            assert!(
                crate::session::decode_session_row(&rows[0]).is_err(),
                "{session_id}: the refusal must leave the undecodable row untouched"
            );
        }
        node.shutdown().await;
    }

    #[tokio::test]
    async fn refusal_survives_unreadable_scoped_request_facts() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(temp.path().join("corrupt-facts.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let node_did = crate::graphql::escape_graphql_string(identity.did());
        for (session_id, sibling_request_id, sibling_created_at) in [
            ("facts-invalid-created-at", "sibling-invalid", "yesterday"),
            (
                "facts-duplicate-roots",
                "refused-dup-root",
                "2026-09-01T00:00:00Z",
            ),
        ] {
            crate::session::ensure_session_with_agent_id_and_requester_did(
                &node,
                session_id,
                identity.did(),
                "agent",
                Some(identity.did()),
            )
            .await
            .unwrap();
            let request_id = if session_id == "facts-duplicate-roots" {
                "refused-dup-root".to_owned()
            } else {
                format!("refused-{session_id}")
            };
            let doc_id =
                refused_pending_request_under_session(&node, &identity, &request_id, session_id)
                    .await;
            let sibling = node
                .execute(&format!(
                    r#"mutation {{ create_AgentRequest(input: {{
                        request_id: "{}", purpose: "normal", node_did: "{node_did}",
                        requester_did: "{node_did}", agent_id: "agent", session_id: "{}",
                        content: "sibling turn", lifecycle_state: "pending",
                        created_at: "{}", retry_count: 0, max_retries: 3, request_hop: 0
                    }}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(sibling_request_id),
                    crate::graphql::escape_graphql_string(session_id),
                    crate::graphql::escape_graphql_string(sibling_created_at),
                ))
                .await;
            assert!(!sibling.has_errors(), "{session_id}: {:?}", sibling.errors);

            super::terminalize_pending_request_rejection(
                &node,
                &doc_id,
                identity.did(),
                "request admission denied: request activates a skill outside its context allowlist",
                "test.admission_rejection_unreadable_facts",
            )
            .await
            .unwrap_or_else(|error| panic!("{session_id}: the refusal must persist: {error}"));

            let selected = node
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                    crate::graphql::escape_graphql_string(&doc_id),
                ))
                .await;
            assert!(!selected.has_errors(), "{:?}", selected.errors);
            assert_eq!(
                selected.data.as_ref().unwrap()["AgentRequest"][0]["lifecycle_state"],
                "failed",
                "{session_id}: the refused request must not stay pending"
            );
            let sessions = node
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::graphql::escape_graphql_string(session_id),
                    crate::session::AGENT_SESSION_FIELDS,
                ))
                .await;
            assert!(!sessions.has_errors(), "{:?}", sessions.errors);
            let rows = sessions.data.as_ref().unwrap()["AgentSession"]
                .as_array()
                .cloned()
                .unwrap();
            assert_eq!(rows.len(), 1, "{session_id}: one session row");
            let session = crate::session::decode_session_row(&rows[0])
                .unwrap()
                .session;
            assert!(
                session.observation.is_none(),
                "{session_id}: a projection the facts cannot order must leave the session \
                 untouched, got {:?}",
                session.observation
            );
        }
        node.shutdown().await;
    }

    #[derive(Clone)]
    struct WarnCapture(Arc<std::sync::Mutex<String>>);

    impl std::io::Write for WarnCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for WarnCapture {
        type Writer = WarnCapture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn version_skewed_refused_row_keeps_its_refusal_and_warns_on_the_skipped_projection() {
        crate::test_support::enable_scoped_event_capture();
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(WarnCapture(captured.clone()))
            .finish();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let temp = tempfile::tempdir().unwrap();
                let identity = KeyIdentity::load_or_create(temp.path().join("skew.key"), None)
                    .unwrap();
                let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().with_node_identity_did(identity.did()).build().await.unwrap());
                ensure_runtime_schemas(&node).await.unwrap();
                let node_did = crate::graphql::escape_graphql_string(identity.did());
                let created = node
                    .execute(&format!(
                        r#"mutation {{ create_AgentRequest(input: {{
                            request_id: "version-skewed", purpose: "normal", node_did: "{node_did}",
                            requester_did: "{node_did}", agent_id: "agent",
                            session_id: "skew-session", content: "run the disallowed skill",
                            lifecycle_state: "pending", created_at: "2026-09-01T00:00:00Z",
                            input: {{ future_field: "written by a newer runtime" }},
                            retry_count: 0, max_retries: 3, request_hop: 0
                        }}) {{ _docID }} }}"#
                    ))
                    .await;
                assert!(!created.has_errors(), "{:?}", created.errors);
                let doc_id = crate::graphql::single_mutation_document(&created, "create_AgentRequest")
                    .unwrap()
                    .unwrap()["_docID"]
                    .as_str()
                    .unwrap()
                    .to_owned();

                super::terminalize_pending_request_rejection(
                    &node,
                    &doc_id,
                    identity.did(),
                    "request admission denied: request activates a skill outside its context allowlist",
                    "test.admission_rejection_version_skew",
                )
                .await
                .unwrap();

                let selected = node
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} AgentSession(filter: {{ session_id: {{ _eq: "skew-session" }} }}) {{ _docID }} }}"#,
                        crate::graphql::escape_graphql_string(&doc_id),
                    ))
                    .await;
                assert!(!selected.has_errors(), "{:?}", selected.errors);
                let data = selected.data.as_ref().unwrap();
                assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "failed");
                assert!(
                    data["AgentSession"].as_array().unwrap().is_empty(),
                    "an undecodable row mints no session"
                );
                node.shutdown().await;
            });
        });
        let logs = captured.lock().unwrap().clone();
        assert!(
            logs.contains("refused row skipped session projection"),
            "the version-skew skip must be observable, log: {logs}"
        );
    }

    #[tokio::test]
    async fn runtime_receipt_authenticates_terminal_expired_child_and_rejects_forgery() {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("receipt.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "receipt-child",
            identity.did(),
            identity.did(),
            "agent-1",
            "receipt-session",
            "original continuation",
            "scheduled",
            "2020-01-01T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_local_control(identity.did(), "receipt-parent"),
        );
        create.caused_by_parent_request_id = Some("receipt-parent".into());
        create.caused_by_parent_request_doc_id = Some("receipt-parent-doc".into());
        create.valid_until = Some("2020-01-01T00:00:01Z".into());
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let query = format!("{{ AgentRequest {{ {} }} }}", super::SIGNED_REQUEST_FIELDS);
        let response = node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let mut row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&response, "AgentRequest")
                .unwrap()
                .unwrap();
        row.lifecycle_state =
            Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed);
        row.deadline = Some("2020-01-01T00:00:01Z".into());
        super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent",
            identity.did(),
        )
        .expect("terminal and expired original receipt remains authenticated");
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "different-parent",
            identity.did(),
        )
        .is_err());
        let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap();
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            foreign.did(),
            "receipt-parent",
            foreign.did(),
        )
        .is_err());
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent",
            foreign.did(),
        )
        .is_err());
        row.content = Some("forged continuation".into());
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent",
            identity.did(),
        )
        .is_err());
        row.content = Some("original continuation".into());
        row.admission_signature = Some("not-a-valid-signature".into());
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent",
            identity.did(),
        )
        .is_err());
        node.shutdown().await;
    }

    #[tokio::test]
    async fn final_verifier_returns_the_exact_fresh_signed_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap());
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(node.as_ref()).await.unwrap();

        let owner = crate::graphql::escape_graphql_string(identity.did());
        let seeded = node.execute(&format!(r#"mutation {{ create_Agent(input: {{agent_id:"agent-1",node_did:"{owner}",inference_profile_id:"inference",enabled:true}}) {{_docID}} }}"#)).await;
        assert!(!seeded.has_errors(), "{:?}", seeded.errors);
        let request_id = uuid::Uuid::new_v4().to_string();
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            "agent-1",
            uuid::Uuid::new_v4().to_string(),
            "durable signed content",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(identity.as_ref(), &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(
            !response.has_errors(),
            "create signed request: {:?}",
            response.errors
        );
        let doc_id = response
            .data
            .as_ref()
            .and_then(|data| {
                data.get("create_AgentRequest")
                    .or_else(|| data.get("add_AgentRequest"))
            })
            .and_then(|value| {
                value.get("_docID").or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                })
            })
            .and_then(serde_json::Value::as_str)
            .expect("created request doc id")
            .to_string();

        let row = super::load_signed_request(node.as_ref(), &doc_id)
            .await
            .unwrap();
        super::verify_request_receipt_signature(&row)
            .expect("original local-self immutable receipt signature");
        let mut queued = super::row_into_agent_request(row, &doc_id).unwrap();
        queued.content = "stale queued content".to_string();

        let runtime_state = node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{_docID: {{_eq:"{}"}}}}, input: {{execution_generation:"previous-claim",max_total_tokens:0}}) {{_docID}} }}"#,
            crate::graphql::escape_graphql_string(&doc_id))).await;
        assert!(!runtime_state.has_errors(), "{:?}", runtime_state.errors);
        let (_authority_owner, authority) = enrollment_authority_channel();
        let verifier = AgentRequestAdmissionVerifier::new(node.clone(), identity, authority);
        let verified = verifier.verify_fresh(&queued, "agent-1").await.unwrap();
        assert_eq!(verified.content, "durable signed content");
        assert_ne!(verified.content, queued.content);
        assert_eq!(
            verified.execution_generation.as_deref(),
            Some("previous-claim")
        );
        assert_eq!(verified.max_total_tokens, Some(0));

        let response = node
            .execute(&format!(
                r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ deadline: "2099-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(&queued.doc_id),
            ))
            .await;
        assert!(
            !response.has_errors(),
            "inject preclaim deadline: {:?}",
            response.errors
        );
        let error = verifier.verify_fresh(&queued, "agent-1").await.unwrap_err();
        assert!(error
            .to_string()
            .contains("caller-authored execution deadline"));
    }

    #[tokio::test]
    async fn runtime_trigger_source_requires_current_trigger_and_target_policy() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        ensure_runtime_schemas(&node).await.unwrap();
        let response = node
            .execute(
                r#"mutation {
                    task: create_Task(input: {
                        task_id: "task-1", node_did: "did:key:target", agent_id: "agent-1",
                        prompt_template: "run", enabled: true
                    }) { _docID }
                    trigger: create_Trigger(input: {
                        trigger_id: "trigger-1", node_did: "did:key:target", task_id: "task-1",
                        source: {kind: "event", event_source_id: "events"}, enabled: true, concurrency: "serial"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "seed trigger policy: {:?}",
            response.errors
        );
        let trigger_doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("trigger"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(|value| value.as_str())
            .expect("trigger doc id")
            .to_string();
        super::verify_automated_trigger_source(
            &node,
            "event",
            "trigger-1",
            Some(&trigger_doc_id),
            "agent-1",
            "did:key:target",
        )
        .await
        .unwrap();

        let response = node
            .execute(
                r#"mutation {
                    update_Trigger(
                        filter: { trigger_id: { _eq: "trigger-1" } },
                        input: { enabled: false }
                    ) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "disable trigger: {:?}",
            response.errors
        );
        assert!(super::verify_automated_trigger_source(
            &node,
            "event",
            "trigger-1",
            Some(&trigger_doc_id),
            "agent-1",
            "did:key:target"
        )
        .await
        .is_err());

        let response = node
            .execute(
                r#"mutation {
                    update_Trigger(
                        filter: { trigger_id: { _eq: "trigger-1" } },
                        input: { enabled: true }
                    ) { _docID }
                    update_Task(
                        filter: { task_id: { _eq: "task-1" } },
                        input: { enabled: false }
                    ) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "disable trigger task: {:?}",
            response.errors
        );
        assert!(super::verify_automated_trigger_source(
            &node,
            "event",
            "trigger-1",
            Some(&trigger_doc_id),
            "agent-1",
            "did:key:target"
        )
        .await
        .is_err());
    }
}
