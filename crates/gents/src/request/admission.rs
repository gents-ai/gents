//! Final, fresh `AgentRequest` authority check. Watcher delivery, replication,
//! queue residence, and caller-authored lineage are not authority.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_admission::{
    project_agent_request_admission_disposition, validate_signing_fields,
    AgentRequestAdmissionDisposition, AgentRequestAdmissionKind, AgentRequestAdmissionObservation,
    AgentRequestAdmissionRecord, RequestPurpose, RuntimeInternalSourceKind, SIGNED_REQUEST_FIELDS,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{AgentRequest, AgentRequestRow};
use serde::Deserialize;

use crate::runtime::agent::p2p_reconcile::EnrollmentAuthorityHandle;
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::store::identity::request_receipt::{
    row_admission, row_signing_fields, verify_auxiliary_request_shape,
};
use crate::store::identity::NodeIdentity;

#[derive(Debug)]
pub(crate) enum AgentRequestAdmissionError {
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
        native_create_authorized: false,
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
    target_agent_did: &str,
) -> AdmissionResult<AgentRequest> {
    let row = load_signed_request_as(node, &request.doc_id, target_agent_did).await?;
    deny_if(
        row.purpose == Some(RequestPurpose::Normal),
        "local-self claim cannot authorize auxiliary request purpose",
    )?;
    let admission = row_admission(&row).map_err(AgentRequestAdmissionError::denied)?;
    if admission.kind == AgentRequestAdmissionKind::Client
        && admission.signer_did != target_agent_did
    {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "atomic requester authorization at agent execution is unavailable"
        )));
    }
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
        && row.agent_did.as_deref() == Some(target_agent_did)
        && validate_signing_fields(&signing_fields).is_ok();
    observation.branch_fields_exact =
        admission.validate_canonical_fields().is_ok() && admission.validate_branch_fields().is_ok();
    observation.pending_deadline_absent = row.deadline.is_none();
    observation.signer_matches_requester =
        row.requester_did.as_deref() == Some(admission.signer_did.as_str());
    observation.requester_matches_target = row.requester_did.as_deref() == row.agent_did.as_deref();
    if admission.kind == AgentRequestAdmissionKind::Client {
        observation.requester_matches_session_scope =
            client_requester_scope_matches(node, &row).await?;
    }
    observation.hop_within_bound = request_hop_admitted(node, &row).await?;
    require_admitted_observation(observation, None)?;
    verify_request_input(
        node,
        &row,
        &admission,
        (identity.did() == target_agent_did).then_some(identity),
    )
    .await?;
    row_into_agent_request(row, &request.doc_id).map_err(AgentRequestAdmissionError::denied)
}

async fn client_requester_scope_matches(
    node: &Arc<EmbeddedNode>,
    row: &AgentRequestRow,
) -> AdmissionResult<bool> {
    let session_scope = crate::session::load_session_requester_scope(
        node,
        required_row_string(row.node_did.as_deref(), "node_did")?,
        required_row_string(row.session_id.as_deref(), "session_id")?,
        required_row_string(row.agent_did.as_deref(), "agent_did")?,
    )
    .await
    .map_err(AgentRequestAdmissionError::unavailable)?;
    Ok(session_scope.is_some() && row.requester_did.as_deref() == session_scope.as_deref())
}

pub async fn sign_agent_request_create(
    identity: &dyn NodeIdentity,
    request: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    validate_signing_fields(&request.signing_fields())?;
    if request.admission.kind == AgentRequestAdmissionKind::Client {
        anyhow::ensure!(
            request
                .session_doc_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
                && request
                    .agent_doc_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty()),
            "client request requires signed physical session and agent identities"
        );
    }
    request.admission.validate_canonical_fields()?;
    anyhow::ensure!(
        request.admission.signer_did == identity.did(),
        "request admission signer does not match authoring identity"
    );
    request.admission.signature.clear();
    request.admission.signature = identity
        .sign(&request.signing_payload())
        .await
        .context("sign AgentRequest admission")?;
    request
        .admission
        .validate_branch_fields()
        .map_err(anyhow::Error::msg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingRejectionOutcome {
    Terminalized,
    NotMatched,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PendingRejectionError {
    #[error("invalid selected agent identity: {0}")]
    InvalidAgent(#[source] anyhow::Error),
    #[error("pending request rejection failed: {0}")]
    Transaction(#[source] anyhow::Error),
}

pub(crate) async fn terminalize_pending_request_rejection(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    node_did: &str,
    agent_did: &str,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    reason: &str,
    operation: &'static str,
) -> std::result::Result<PendingRejectionOutcome, PendingRejectionError> {
    if node.node_identity_did() != Some(node_did) || agent_did == node_did {
        return Err(PendingRejectionError::InvalidAgent(anyhow::anyhow!(
            "pending rejection requires the local runtime's distinct managed Agent"
        )));
    }
    let actor = identity::Did::new(agent_did)
        .map_err(|error| PendingRejectionError::InvalidAgent(error.into()))?;
    let access = crate::ConfigAccess::local_managed_agent(node.clone(), actor, agent_identity)
        .map_err(|error| PendingRejectionError::InvalidAgent(error.into()))?;
    let escaped_agent = escape_graphql_string(agent_did);
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
                    agent_did: {{ _eq: "{escaped_agent}" }},
                    lifecycle_state: {{ _eq: "{pending}" }}
                }},
                input: {{
                    lifecycle_state: "{failed}",
                    failure_reason: "{failure_reason}",
                    terminalized_at: "{terminalized_at}",
                    terminal_redrive_attempts: 0,
                    terminal_output: $terminal_output
                }}
            ) {{ _docID request_id purpose node_did requester_did agent_did session_id session_doc_id agent_doc_id content
                 input created_at caused_by_parent_request_doc_id
                 workspace_id workspace_owner_node_did }}
        }}"#,
        pending = RequestLifecycleState::Pending.as_str(),
        failed = RequestLifecycleState::Failed.as_str()
    );
    let mutation = &mutation;
    access.transact_idempotent(
        crate::store::config_client::IdempotentTransactionRetry::Standard,
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
                let rows = crate::store::graphql::mutation_documents(&response, "update_AgentRequest")?
                    .context("pending rejection mutation omitted its result")?;
                anyhow::ensure!(rows.len() <= 1, "pending rejection updated multiple requests");
                for row in rows {
                    crate::tools::workspace::release_terminal_writer_binding(txn, row).await?;
                    if let Some(request_id) = row["request_id"].as_str() {
                        crate::triggers::engine::durable::publish_request_outcome(
                            txn, owner, request_id, RequestLifecycleState::Failed, reason, now,
                        )
                        .await?;
                    }
                    match serde_json::from_value::<AgentRequestRow>(row.clone())
                        .map_err(anyhow::Error::from)
                        .and_then(|row| {
                            gents_protocol::row::AgentRequest::try_from(row)
                                .map_err(anyhow::Error::from)
                        })
                    {
                        Ok(request) => {
                            crate::request::lifecycle::materialize::project_refused_request_session_in_txn(
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
                Ok::<_, anyhow::Error>(if rows.is_empty() {
                    PendingRejectionOutcome::NotMatched
                } else {
                    PendingRejectionOutcome::Terminalized
                })
            })
        },
    )
    .await
    .map_err(PendingRejectionError::Transaction)
}

pub async fn sign_agent_request_create_as_registered_target(
    request: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let identity = crate::store::identity::RegisteredIdentity::from_registered_did(
        request.agent_did.clone(),
        None,
    )
    .context("load registered target agent identity for AgentRequest authoring")?;
    sign_agent_request_create(&identity, request).await
}

/// Immutable signed admission retained until the native claim transaction.
#[derive(Debug, Clone)]
pub(crate) struct ClientClaimAdmission {
    row: AgentRequestRow,
}

/// Exact signed client provenance retained by a successful native claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedClientProvenance {
    pub(crate) signer_did: String,
    pub(crate) request_doc_id: String,
    pub(crate) session_doc_id: String,
    pub(crate) agent_doc_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientClaimAuthorization {
    Allowed,
    Denied,
    Unavailable,
}

impl ClientClaimAdmission {
    pub(crate) fn claimed_provenance(
        &self,
        request: &AgentRequest,
    ) -> Option<ClaimedClientProvenance> {
        let admission = row_admission(&self.row).ok()?;
        let request_doc_id = self.row.doc_id.as_ref()?;
        if admission.kind != AgentRequestAdmissionKind::Client
            || request_doc_id != &request.doc_id
            || self.row.request_id != request.request_id
            || self.row.agent_did.as_deref() != Some(request.agent_did.as_str())
            || self.row.session_id.as_deref() != Some(request.session_id.as_str())
        {
            return None;
        }
        Some(ClaimedClientProvenance {
            signer_did: admission.signer_did,
            request_doc_id: request_doc_id.clone(),
            session_doc_id: self
                .row
                .session_doc_id
                .clone()
                .filter(|id| !id.is_empty())?,
            agent_doc_id: self.row.agent_doc_id.clone().filter(|id| !id.is_empty())?,
        })
    }

    #[cfg(test)]
    pub(crate) fn alter_signed_content_for_test(&mut self) {
        self.row.content = Some("altered verified input".into());
    }

    /// Fence relationship readiness and rebind cross-actor native permissions
    /// to the claim transaction.
    pub(crate) async fn authorize_in_txn(
        &self,
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
        request: &AgentRequest,
    ) -> Result<ClientClaimAuthorization> {
        use ClientClaimAuthorization::{Allowed, Denied, Unavailable};
        let doc = escape_graphql_string(&request.doc_id);
        let response = txn.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ {SIGNED_REQUEST_FIELDS} relationships_pending }} }}"#,
        )).await?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("claim admission query omitted rows")?;
        if rows.len() != 1 {
            return Ok(Unavailable);
        }
        if rows[0]["relationships_pending"].as_bool() != Some(false) {
            return Ok(Unavailable);
        }
        let row: AgentRequestRow = serde_json::from_value(rows[0].clone())?;
        let retained = row_admission(&self.row)?;
        let Some(session_doc) = self.row.session_doc_id.as_deref() else {
            return Ok(Unavailable);
        };
        let Some(agent_doc) = self.row.agent_doc_id.as_deref() else {
            return Ok(Unavailable);
        };
        let session = escape_graphql_string(session_doc);
        let response = txn.execute(&format!(
            r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{session}" }} }}, limit: 2) {{ _docID relationships_pending }} }}"#,
        )).await?;
        let Some(sessions) = response["data"]["AgentSession"].as_array() else {
            return Ok(Unavailable);
        };
        if !matches!(sessions.as_slice(), [row] if row["_docID"].as_str() == Some(session_doc)
            && row["relationships_pending"].as_bool() == Some(false))
        {
            return Ok(Unavailable);
        }
        let current = row_admission(&row);
        let bound = current.as_ref().is_ok_and(|current| {
            current.kind == AgentRequestAdmissionKind::Client
                && current.signer_did == retained.signer_did
                && current.signature == retained.signature
                && row_signing_fields(&row)
                    .ok()
                    .zip(row_signing_fields(&self.row).ok())
                    .is_some_and(|(live, signed)| {
                        current.signing_payload(&live) == retained.signing_payload(&signed)
                    })
                && row.doc_id.as_deref() == Some(request.doc_id.as_str())
                && row.request_id == request.request_id
                && row.node_did.as_deref() == Some(request.node_did.as_str())
                && row.session_id.as_deref() == Some(request.session_id.as_str())
                && row.agent_did.as_deref() == Some(request.agent_did.as_str())
        });
        if retained.signer_did == request.agent_did {
            return Ok(if bound { Allowed } else { Unavailable });
        }
        let owners = txn
            .document_owners("AgentRequest", std::slice::from_ref(&request.doc_id))
            .await?;
        let Some(owner) = owners.first().and_then(|entry| entry.owner.as_ref()) else {
            return Ok(Unavailable);
        };
        let owner_matches = owners.len() == 1 && owner.as_str() == retained.signer_did;
        let Some(session) =
            crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                txn,
                &request.node_did,
                &request.session_id,
            )
            .await?
        else {
            return Ok(Unavailable);
        };
        if session.doc_id != session_doc {
            return Ok(Unavailable);
        }
        let session_matches = session.session.agent_did == request.agent_did
            && session.session.requester_did == request.requester_did;
        let agent = escape_graphql_string(agent_doc);
        let target = txn.execute(&format!(r#"{{ Agent(filter: {{ _docID: {{ _eq: "{agent}" }} }}, limit: 2) {{ _docID node_did agent_did enabled }} }}"#)).await?;
        let agents = target["data"]["Agent"]
            .as_array()
            .context("claim agent query omitted rows")?;
        if agents.len() != 1 {
            return Ok(Unavailable);
        }
        let agent_matches = agents[0]["_docID"].as_str() == Some(agent_doc)
            && agents[0]["node_did"].as_str() == Some(request.node_did.as_str())
            && agents[0]["agent_did"].as_str() == Some(request.agent_did.as_str())
            && agents[0]["enabled"].as_bool() != Some(false);
        let subject = identity::Did::new(&retained.signer_did)?;
        let named = |permission: &str| defra_node::DocumentAccessMode::Named {
            permission: permission.to_owned(),
            node_permission: acp::nac::NodePermission::DocumentUpdate,
        };
        let session_permission = txn
            .observe_document_access(
                &subject,
                "AgentSession",
                &[session_doc.to_owned()],
                named("update"),
            )
            .await?;
        let agent_permission = txn
            .observe_document_access(&subject, "Agent", &[agent_doc.to_owned()], named("invoke"))
            .await?;
        use defra_node::DocumentAccessDecision::{
            Allowed as AccessAllowed, Denied as AccessDenied,
        };
        let [session_permission @ (AccessAllowed | AccessDenied)] = session_permission.as_slice()
        else {
            return Ok(Unavailable);
        };
        let [agent_permission @ (AccessAllowed | AccessDenied)] = agent_permission.as_slice()
        else {
            return Ok(Unavailable);
        };
        if !bound
            || !owner_matches
            || !session_matches
            || !agent_matches
            || *session_permission == AccessDenied
            || *agent_permission == AccessDenied
        {
            return Ok(Denied);
        }
        Ok(Allowed)
    }
}

#[derive(Clone)]
pub(crate) struct AgentRequestAdmissionVerifier {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    enrollment: EnrollmentAuthorityHandle,
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
            enrollment,
        }
    }

    pub(crate) async fn verify_fresh(
        &self,
        request: &AgentRequest,
        target_agent_did: &str,
    ) -> AdmissionResult<AgentRequest> {
        self.verify_fresh_with_observation(request, target_agent_did, None, false, None)
            .await
            .map(|(request, _)| request)
    }

    #[cfg(test)]
    pub(crate) async fn verify_fresh_for_claim(
        &self,
        request: &AgentRequest,
        target_agent_did: &str,
    ) -> AdmissionResult<(AgentRequest, Option<ClientClaimAdmission>)> {
        self.verify_fresh_with_observation(request, target_agent_did, None, true, None)
            .await
    }

    pub(crate) async fn verify_fresh_for_claim_with_identity(
        &self,
        request: &AgentRequest,
        agent_identity: &dyn NodeIdentity,
    ) -> AdmissionResult<(AgentRequest, Option<ClientClaimAdmission>)> {
        self.verify_fresh_with_observation(
            request,
            agent_identity.did(),
            None,
            true,
            Some(agent_identity),
        )
        .await
    }

    pub(crate) async fn verify_folded_for_claim(
        &self,
        document: &str,
        agent_identity: &dyn NodeIdentity,
    ) -> AdmissionResult<(AgentRequest, Option<ClientClaimAdmission>)> {
        let row = load_signed_request_as(&self.node, document, agent_identity.did()).await?;
        let request = AgentRequest::try_from(row).map_err(AgentRequestAdmissionError::denied)?;
        self.verify_fresh_for_claim_with_identity(&request, agent_identity)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn verify_fresh_for_claim_at(
        &self,
        request: &AgentRequest,
        agent_identity: &dyn NodeIdentity,
        observed_at: chrono::DateTime<Utc>,
    ) -> AdmissionResult<(AgentRequest, Option<ClientClaimAdmission>)> {
        self.verify_fresh_with_observation(
            request,
            agent_identity.did(),
            Some(observed_at),
            true,
            Some(agent_identity),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn verify_fresh_at(
        &self,
        request: &AgentRequest,
        target_agent_did: &str,
        observed_at: chrono::DateTime<Utc>,
    ) -> AdmissionResult<AgentRequest> {
        self.verify_fresh_with_observation(
            request,
            target_agent_did,
            Some(observed_at),
            false,
            None,
        )
        .await
        .map(|(request, _)| request)
    }

    async fn verify_fresh_with_observation(
        &self,
        request: &AgentRequest,
        target_agent_did: &str,
        test_observed_at: Option<chrono::DateTime<Utc>>,
        allow_cross_actor_client_claim: bool,
        agent_identity: Option<&dyn NodeIdentity>,
    ) -> AdmissionResult<(AgentRequest, Option<ClientClaimAdmission>)> {
        let row = load_signed_request_as(&self.node, &request.doc_id, target_agent_did).await?;
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
            && row.agent_did.as_deref() == Some(target_agent_did)
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
            verify_auxiliary_request_shape(&row, &admission)
                .map_err(AgentRequestAdmissionError::denied)?;
        }
        // A default RequestInput is persisted as an absent field, so an absent
        // input is the canonical empty input, not a session creation.
        let local_agent_message = admission.kind == AgentRequestAdmissionKind::Client
            && row.caused_by_parent_tool_call_doc_id.is_some()
            && row
                .input
                .as_ref()
                .is_none_or(|input| input.session_creation.is_none());
        let mut denied = None;
        if admission.kind == AgentRequestAdmissionKind::Peer
            || (matches!(
                admission.kind,
                AgentRequestAdmissionKind::Client
                    | AgentRequestAdmissionKind::Enrollment
                    | AgentRequestAdmissionKind::LocalSelf
            ) && row.agent_did.as_deref() != Some(admission.signer_did.as_str())
                && !(allow_cross_actor_client_claim
                    && admission.kind == AgentRequestAdmissionKind::Client
                    && row.purpose == Some(RequestPurpose::Normal)
                    && client_claim_queue_candidate(row.input.as_ref(), local_agent_message)))
        {
            return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
                "atomic requester authorization at agent execution is unavailable"
            )));
        }
        match admission.kind {
            AgentRequestAdmissionKind::Client => {
                observation.requester_matches_session_scope =
                    client_requester_scope_matches(&self.node, &row).await?;
                if local_agent_message {
                    let source = crate::session::origin::verified_agent_source(
                        &crate::ConfigAccess::local_as(
                            self.node.clone(),
                            identity::Did::new(admission.signer_did.clone())
                                .map_err(AgentRequestAdmissionError::denied)?,
                        ),
                        &row,
                    )
                    .await
                    .map_err(AgentRequestAdmissionError::unavailable)?;
                    if !source.is_some_and(|source| {
                        Some(source.node_did.as_str()) == row.node_did.as_deref()
                    }) {
                        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
                            "local agent-message source is unavailable"
                        )));
                    }
                }
            }
            AgentRequestAdmissionKind::LocalSelf => {
                observation.signer_matches_requester =
                    row.requester_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.agent_did.as_deref();
                if observation.signer_matches_requester && !observation.requester_matches_target {
                    observation.source_binding_current =
                        crate::session::origin::verified_agent_source(
                            &crate::ConfigAccess::local_as(
                                self.node.clone(),
                                identity::Did::new(admission.signer_did.clone())
                                    .map_err(AgentRequestAdmissionError::denied)?,
                            ),
                            &row,
                        )
                        .await
                        .map_err(AgentRequestAdmissionError::unavailable)?
                        .is_some_and(|source| {
                            Some(source.node_did.as_str()) == row.node_did.as_deref()
                        });
                }
            }
            AgentRequestAdmissionKind::Peer => {
                observation.signer_matches_requester =
                    row.requester_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.agent_did.as_deref();
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
                    row.agent_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.agent_did.as_deref();
                observation.target_runtime_attestation_valid = issuer == row.agent_did.as_deref();
                let target = required_row_string(row.node_did.as_deref(), "node_did")?;
                let session_scope = if row.purpose.is_some_and(RequestPurpose::is_auxiliary) {
                    None
                } else {
                    crate::session::load_session_requester_scope(
                        &self.node,
                        target,
                        required_row_string(row.session_id.as_deref(), "session_id")?,
                        required_row_string(row.agent_did.as_deref(), "agent_did")?,
                    )
                    .await
                    .map_err(AgentRequestAdmissionError::unavailable)?
                };
                observation.requester_matches_session_scope = row.requester_did.as_deref()
                    == Some(
                        session_scope
                            .as_deref()
                            .unwrap_or(required_row_string(row.agent_did.as_deref(), "agent_did")?),
                    );
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
                        target_agent_did,
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
        verify_request_input(&self.node, &row, &admission, agent_identity).await?;
        let client_claim = (allow_cross_actor_client_claim
            && admission.kind == AgentRequestAdmissionKind::Client)
            .then(|| ClientClaimAdmission { row: row.clone() });
        let request = row_into_agent_request(row, &request.doc_id)
            .map_err(AgentRequestAdmissionError::denied)?;
        Ok((request, client_claim))
    }
}

fn client_claim_queue_candidate(
    input: Option<&gents_protocol::request_input::RequestInput>,
    local_agent_message: bool,
) -> bool {
    use gents_protocol::request_input::QueueSource;
    input.is_none_or(|input| {
        input.queue.as_ref().is_none_or(|queue| {
            queue.source == QueueSource::User
                || (local_agent_message && queue.source == QueueSource::Steering)
                || (queue.source == QueueSource::Goal && input.goal_continuation.is_some())
        })
    })
}

async fn verify_request_input(
    node: &Arc<EmbeddedNode>,
    row: &AgentRequestRow,
    admission: &AgentRequestAdmissionRecord,
    agent_identity: Option<&dyn NodeIdentity>,
) -> AdmissionResult<()> {
    use gents_protocol::request_input::QueueSource;
    let input = row.input.as_ref().unwrap_or(&DEFAULT_REQUEST_INPUT);
    if row.purpose.is_some_and(RequestPurpose::is_auxiliary) {
        return deny_if(
            input == &*DEFAULT_REQUEST_INPUT,
            "auxiliary request cannot carry ordinary request input",
        );
    }
    validate_authored_content(input)?;
    let owner = required_row_string(row.node_did.as_deref(), "node_did")?;
    let agent = required_row_string(row.agent_did.as_deref(), "agent_did")?;
    let (context, tools, _) = load_request_context(node, owner, agent).await?;
    let skills = context
        .as_ref()
        .map(|context| context.skill_ids.as_slice())
        .unwrap_or_default();
    validate_authored_skills(input, skills)?;
    let request =
        AgentRequest::try_from(row.clone()).map_err(AgentRequestAdmissionError::denied)?;
    let artifact_requested = tools
        .as_ref()
        .and_then(|tools| tools.host.as_ref())
        .and_then(|host| host.bash.as_ref())
        .and_then(|bash| bash.execution_mode)
        == Some(crate::tools::native::CommandExecutionMode::ArtifactWrite);
    if let Some(agent_identity) = agent_identity {
        crate::tools::workspace::validate_request_workspace_input_with_identity(
            node,
            agent_identity,
            &request,
            artifact_requested,
        )
        .await
        .map_err(AgentRequestAdmissionError::denied)?;
    } else if request
        .workspace_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
    {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "workspace admission requires the selected held Agent"
        )));
    } else {
        crate::tools::workspace::validate_unbound_request_workspace_input(
            &request,
            artifact_requested,
        )
        .map_err(AgentRequestAdmissionError::denied)?;
    }
    let runtime_control = admission.kind == AgentRequestAdmissionKind::RuntimeInternal
        && admission.runtime_source_kind == Some(RuntimeInternalSourceKind::LocalControl);
    let client_goal =
        admission.kind == AgentRequestAdmissionKind::Client && input.goal_continuation.is_some();
    if let Some(queue) = &input.queue {
        let session_message_steering = queue.source == QueueSource::Steering
            && row.caused_by_parent_request_doc_id.is_some()
            && row.caused_by_parent_tool_call_doc_id.is_some();
        deny_if(
            queue.source == QueueSource::User
                || runtime_control
                || session_message_steering
                || (client_goal && queue.source == QueueSource::Goal),
            "runtime queue source requires authenticated local-control issuance",
        )?;
        if queue.position.is_some() {
            let actor = identity::Did::new(agent).map_err(AgentRequestAdmissionError::denied)?;
            crate::store::config_client::ConfigAccess::transact_local_readonly(
                node,
                Some(actor),
                "admission.queue_position",
                |txn| {
                    Box::pin(async move {
                        crate::request::lifecycle::queue::validate_position(txn, row).await
                    })
                },
            )
            .await
            .map_err(|error| {
                if error.is::<crate::request::lifecycle::queue::InvalidQueuePosition>() {
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
            (runtime_control || client_goal) && input.goal_continuation.is_some(),
            "goal continuation requires authenticated original issuance facts",
        )?;
        let parent = load_exact_parent_request(
            node,
            required_row_string(
                row.caused_by_parent_request_doc_id.as_deref(),
                "parent document ID",
            )?,
            agent,
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

pub(crate) fn validate_authored_content(
    input: &gents_protocol::request_input::RequestInput,
) -> AdmissionResult<()> {
    deny_if(
        input.captured_context.iter().all(|context| context.is_well_formed()),
        "captured context requires an ordered UTF-16 range and a positive buffer version when present",
    )?;
    input
        .validate_attachments()
        .map_err(AgentRequestAdmissionError::denied)
}

pub(crate) fn validate_authored_skills(
    input: &gents_protocol::request_input::RequestInput,
    skills: &[String],
) -> AdmissionResult<()> {
    deny_if(
        input
            .selected_skill_ids
            .iter()
            .all(|id| skills.contains(id)),
        "request activates a skill outside its context allowlist",
    )
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
    Ok(max_request_hop.admit(hop).unwrap_or(false))
}

pub(crate) async fn max_request_hop(
    node: &EmbeddedNode,
    node_did: &str,
) -> anyhow::Result<gents_protocol::request_lifecycle::RequestHopCapObservation> {
    Ok(
        gents_protocol::request_lifecycle::RequestHopCapObservation::of(
            crate::documents::load_node(node, node_did)
                .await?
                .map(|node_doc| node_doc.max_request_hop),
        ),
    )
}

fn request_workspace(row: &AgentRequestRow) -> crate::request::lifecycle::WorkspaceLineage {
    crate::request::lifecycle::WorkspaceLineage {
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
    target_agent_did: &str,
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
            let parent = load_exact_parent_request(&node, parent_doc_id, target_agent_did).await?;
            deny_if(
                parent.request_id == source && parent.node_did == row.node_did,
                "local-control parent document does not exactly own the source",
            )?;
            if purpose.is_auxiliary() {
                return deny_if(
                    parent.session_id == row.session_id
                        && parent.agent_did.as_deref() == Some(target_agent_did),
                    "auxiliary request parent does not match its session and agent",
                );
            }
            deny_if(
                parent.session_id == row.session_id
                    && parent
                        .requester_did
                        .as_deref()
                        .or(parent.agent_did.as_deref())
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
                target_agent_did,
                required_row_string(row.node_did.as_deref(), "node_did")?,
            )
            .await
        }
    }
}

async fn load_exact_parent_request(
    node: &Arc<EmbeddedNode>,
    source_doc_id: &str,
    actor: &str,
) -> AdmissionResult<AgentRequestRow> {
    let row = load_signed_request_as(node, source_doc_id, actor).await?;
    deny_if(
        row.doc_id.as_deref() == Some(source_doc_id),
        "runtime source parent physical document binding changed",
    )?;
    Ok(row)
}

pub(crate) async fn load_request_context(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
) -> AdmissionResult<(
    Option<crate::documents::AgentContext>,
    Option<crate::documents::Tools>,
    Vec<crate::documents::AgentTargetDocument>,
)> {
    use crate::store::config_client::ConfigAccess;
    let owner = node_did.to_owned();
    let agent_did = agent_did.to_owned();
    let actor =
        ::identity::Did::new(&agent_did).map_err(AgentRequestAdmissionError::unavailable)?;
    ConfigAccess::transact_local_readonly(
        node,
        Some(actor),
        "request_admission.context",
        move |txn| {
            let owner = owner.clone();
            let agent_did = agent_did.clone();
            Box::pin(async move { load_request_context_in_txn(txn, &owner, &agent_did).await })
        },
    )
    .await
    .map_err(classify_context_error)
}

pub(crate) fn classify_context_error(error: anyhow::Error) -> AgentRequestAdmissionError {
    match error.downcast::<AgentRequestAdmissionError>() {
        Ok(error) => error,
        Err(error) => AgentRequestAdmissionError::unavailable(error),
    }
}

/// [`load_request_context`] inside `txn`; a refusal is an
/// [`AgentRequestAdmissionError`] in the returned error.
pub(crate) async fn load_request_context_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    agent_did: &str,
) -> anyhow::Result<(
    Option<crate::documents::AgentContext>,
    Option<crate::documents::Tools>,
    Vec<crate::documents::AgentTargetDocument>,
)> {
    use crate::documents::{Agent, AgentContext, AgentTargetDocument, Tools};
    use crate::store::collection::Collection;
    use crate::store::config_client::read_desired_state_document_in_txn as read;
    let agent: Agent = serde_json::from_value(
        read(txn, Collection::Agent, owner, agent_did)
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
    target_agent_did: &str,
    node_did: &str,
) -> AdmissionResult<()> {
    #[derive(Deserialize)]
    struct TriggerRow {
        trigger_id: String,
        node_did: String,
        task_id: String,
        source: crate::documents::TriggerSource,
        enabled: bool,
    }
    #[derive(Deserialize)]
    struct TaskRow {
        agent_did: String,
        enabled: bool,
    }
    let doc = required_row_string(trigger_doc_id, "trigger document ID")?;
    let response = graphql_with_transaction_retry(node, &format!(
        r#"{{ Trigger(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ trigger_id node_did task_id source enabled }} }}"#,
        escape_graphql_string(doc),
    ), "reload runtime trigger").await.map_err(AgentRequestAdmissionError::unavailable)?;
    let triggers: Vec<TriggerRow> = crate::store::graphql::rows(&response, "Trigger")
        .map_err(AgentRequestAdmissionError::denied)?;
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
                (crate::documents::TriggerSource::Event { .. }, "event")
                    | (crate::documents::TriggerSource::Schedule { .. }, "schedule")
            ),
        "runtime trigger physical source, node, kind, or availability changed",
    )?;
    let response = graphql_with_transaction_retry(node, &format!(
        r#"{{ Task(filter: {{ task_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }} }}, limit: 2) {{ agent_did enabled }} }}"#,
        escape_graphql_string(&trigger.task_id), escape_graphql_string(node_did),
    ), "reload runtime trigger task").await.map_err(AgentRequestAdmissionError::unavailable)?;
    let tasks: Vec<TaskRow> = crate::store::graphql::rows(&response, "Task")
        .map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        tasks.len() == 1 && tasks[0].enabled && tasks[0].agent_did == target_agent_did,
        "runtime trigger task is missing, disabled, ambiguous, or targets another agent",
    )
}

static DEFAULT_REQUEST_INPUT: std::sync::LazyLock<gents_protocol::request_input::RequestInput> =
    std::sync::LazyLock::new(Default::default);

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

async fn load_signed_request_as(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    actor: &str,
) -> AdmissionResult<AgentRequestRow> {
    let actor = identity::Did::new(actor).map_err(AgentRequestAdmissionError::denied)?;
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {SIGNED_REQUEST_FIELDS} }} }}"#,
        escape_graphql_string(doc_id),
    );
    let response = crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(actor),
        "request.admission.read_as_agent",
        |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        },
    )
    .await
    .map_err(AgentRequestAdmissionError::unavailable)?;
    let row = response
        .pointer("/data/AgentRequest/0")
        .cloned()
        .ok_or_else(|| {
            AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
                "AgentRequest is unavailable to selected agent"
            ))
        })?;
    serde_json::from_value(row).map_err(AgentRequestAdmissionError::denied)
}

#[cfg(test)]
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
    crate::store::graphql::first_row(&response, "AgentRequest")
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
pub(crate) async fn load_request_for_admission_test_as(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    actor: &str,
) -> Result<AgentRequest> {
    let row = load_signed_request_as(node, doc_id, actor)
        .await
        .map_err(anyhow::Error::from)?;
    row_into_agent_request(row, doc_id)
}

#[cfg(test)]
mod title_tests;

#[cfg(test)]
mod captured_context_tests;

#[cfg(test)]
mod relationship_readiness_tests;

#[cfg(test)]
mod unattended_tests;

#[cfg(test)]
pub(crate) mod workspace_cleanup_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::AgentRequestAdmissionVerifier;
    use crate::runtime::agent::p2p_reconcile::enrollment_authority_channel;
    use crate::store::identity::{KeyIdentity, NodeIdentity};
    use crate::store::schema::ensure_runtime_schemas;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

    #[test]
    fn generated_client_claim_queue_candidates_match_the_model() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().request_input_cases;
        for case in cases {
            let input: gents_protocol::request_input::RequestInput =
                serde_json::from_value(case["input"].clone()).unwrap();
            for (agent_message, expected) in [
                (false, "expected_client_claim_queue_candidate"),
                (true, "expected_agent_message_claim_queue_candidate"),
            ] {
                assert_eq!(
                    super::client_claim_queue_candidate(Some(&input), agent_message),
                    case[expected].as_bool().unwrap(),
                    "{}: local agent message {agent_message}",
                    case["name"]
                );
            }
        }
        for name in [
            "client-goal-original-edge-preserved",
            "goal-source-needs-owned-admission",
            "client-unrelated-runtime-queue-denied",
            "client-local-agent-message-steering-candidate",
        ] {
            assert!(cases.iter().any(|case| case["name"] == name), "{name}");
        }
        assert!(super::client_claim_queue_candidate(None, false));
    }

    #[tokio::test]
    async fn pending_rejection_requires_the_selected_managed_agent() {
        let dir = tempfile::tempdir().unwrap();
        let root = KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap();
        let other_root = KeyIdentity::load_or_create(dir.path().join("other.key"), None).unwrap();
        let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let other_binding = crate::store::identity::provision_agent_identity(&other_root).unwrap();
        let other_agent =
            crate::store::identity::resolve_agent_identity(&other_root, &other_binding).unwrap();
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(root.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let denied = super::terminalize_pending_request_rejection(
            &node,
            "missing-doc",
            root.did(),
            agent.did(),
            other_agent.as_ref(),
            "rejected",
            "test.rejection_foreign_agent",
        )
        .await;
        assert!(matches!(
            denied,
            Err(super::PendingRejectionError::InvalidAgent(_))
        ));
        let outcome = super::terminalize_pending_request_rejection(
            &node,
            "missing-doc",
            root.did(),
            agent.did(),
            agent.as_ref(),
            "rejected",
            "test.rejection_selected_agent",
        )
        .await
        .unwrap();
        assert_eq!(outcome, super::PendingRejectionOutcome::NotMatched);
        node.shutdown().await.unwrap();
    }

    /// Signed admission fixture: a test home whose ACP policies are installed,
    /// whose default managed Agent holds its projected baseline grants, and whose
    /// setup rows are written through the runtime owner's access.
    struct AdmissionFixture {
        db: crate::support::TestDb,
        runtime: Arc<dyn NodeIdentity>,
        agent: Arc<dyn NodeIdentity>,
        admin: crate::store::config_client::ConfigAccess,
        /// The configuring User's client-signed access; it owns Agent configuration.
        user: crate::store::config_client::ConfigAccess,
    }

    impl AdmissionFixture {
        async fn new(name: &str) -> Self {
            let db = crate::support::test_db(name).await;
            let (db, _user, user, binding) =
                crate::test_support::signed_test_node_from_db(db, name).await;
            let runtime = db.node_identity.clone();
            let agent =
                crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
            let admin = crate::store::config_client::ConfigAccess::local_as(
                db.node.clone(),
                identity::Did::new(runtime.did()).unwrap(),
            );
            let runtime_did = runtime.did().to_owned();
            let agent_did = agent.did().to_owned();
            admin
                .transact_write("test.admission_fixture_executor", |txn| {
                    let runtime_did = &runtime_did;
                    let agent_did = &agent_did;
                    Box::pin(async move {
                        let (agent_doc, _) =
                            crate::store::config_client::read_desired_state_record_in_txn(
                                txn,
                                crate::Collection::Agent,
                                runtime_did,
                                agent_did,
                            )
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("fixture Agent document is missing"))?;
                        txn.add_actor_relationship(
                            "Agent",
                            &agent_doc,
                            "executor",
                            &identity::Did::new(agent_did)?,
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
            Self {
                db,
                runtime,
                agent,
                admin,
                user,
            }
        }

        fn node(&self) -> &Arc<defra_node::Node> {
            &self.db.node
        }

        fn local_self_create(
            &self,
            request_id: &str,
            session_id: &str,
            agent_did: &str,
            input: Option<serde_json::Value>,
        ) -> AgentRequestCreate {
            let mut create = AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                request_id,
                self.runtime.did(),
                self.runtime.did(),
                agent_did,
                session_id,
                "run the disallowed skill",
                "interactive",
                "2026-09-01T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(self.runtime.did()),
            );
            if let Some(input) = input {
                create.input = serde_json::from_value(input).unwrap();
            }
            create
        }

        /// Signs `create` as the runtime and writes it through the runtime's access.
        async fn create_signed_request(&self, mut create: AgentRequestCreate) -> String {
            crate::sign_agent_request_create(self.runtime.as_ref(), &mut create)
                .await
                .unwrap();
            self.insert_signed_request(&create).await
        }

        /// Signs `create` as the managed Agent. Only a request the Agent authors for
        /// itself (requester and signer both its DID) is admitted as local-self.
        async fn create_agent_signed_request(&self, mut create: AgentRequestCreate) -> String {
            crate::sign_agent_request_create(self.agent.as_ref(), &mut create)
                .await
                .unwrap();
            self.insert_signed_request(&create).await
        }

        fn agent_self_create(
            &self,
            request_id: &str,
            session_id: &str,
            content: &str,
            input: Option<serde_json::Value>,
        ) -> AgentRequestCreate {
            let agent_did = self.agent.did();
            let mut create = AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                request_id,
                self.runtime.did(),
                agent_did,
                agent_did,
                session_id,
                content,
                "interactive",
                "2026-09-01T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(agent_did),
            );
            if let Some(input) = input {
                create.input = serde_json::from_value(input).unwrap();
            }
            create
        }

        /// Writes a signed request through the runtime's access and stages the
        /// `working_agent` edge that lets its target Agent read and terminalize it.
        async fn insert_signed_request(&self, create: &AgentRequestCreate) -> String {
            let mutation = create.graphql_mutation().unwrap();
            let doc_id = self.write_as_runtime(mutation, "AgentRequest").await;
            self.grant_working_agent(&doc_id, &create.agent_did).await;
            doc_id
        }

        async fn grant_working_agent(&self, doc_id: &str, agent_did: &str) {
            self.admin
                .add_actor_relationship(
                    "AgentRequest",
                    doc_id,
                    "working_agent",
                    &identity::Did::new(agent_did).unwrap(),
                )
                .await
                .unwrap();
        }

        /// The managed Agent owns the sessions it projects into; fixture sessions
        /// and their observations are written and read under that same actor.
        fn agent_access(&self) -> crate::store::config_client::ConfigAccess {
            crate::store::config_client::ConfigAccess::local_managed_agent(
                self.db.node.clone(),
                identity::Did::new(self.agent.did()).unwrap(),
                self.agent.as_ref(),
            )
            .unwrap()
        }

        async fn ensure_agent_session(&self, session_id: &str, requester_did: Option<&str>) {
            let node_did = self.runtime.did().to_owned();
            let agent_did = self.agent.did().to_owned();
            self.agent_access()
                .transact_write("test.admission_fixture_session", |txn| {
                    let node_did = &node_did;
                    let agent_did = &agent_did;
                    Box::pin(async move {
                        crate::session::ensure_session_in_txn(
                            txn,
                            session_id,
                            node_did,
                            agent_did,
                            requester_did,
                            None,
                            None,
                            "2026-09-01T00:00:00Z",
                        )
                        .await
                    })
                })
                .await
                .unwrap();
        }

        async fn write_as_agent(&self, mutation: String, collection: &'static str) -> String {
            self.agent_access()
                .transact_write("test.admission_fixture_agent_write", |txn| {
                    let mutation = &mutation;
                    Box::pin(async move {
                        let response = txn.execute(mutation).await?;
                        let doc_id = crate::store::graphql::created_doc_id(&response, collection)?;
                        Ok::<_, anyhow::Error>(doc_id)
                    })
                })
                .await
                .unwrap()
        }

        async fn read_as_agent(&self, document: &str) -> defra_node::QueryResponse {
            self.agent_access()
                .transact_readonly("test.admission_fixture_agent_read", |txn| {
                    Box::pin(async move { txn.execute_local_response(document).await })
                })
                .await
                .unwrap()
        }

        async fn write_as_runtime(&self, mutation: String, collection: &'static str) -> String {
            self.admin
                .transact_write("test.admission_fixture_write", |txn| {
                    let mutation = &mutation;
                    Box::pin(async move {
                        let response = txn.execute(mutation).await?;
                        let doc_id = crate::store::graphql::created_doc_id(&response, collection)?;
                        Ok::<_, anyhow::Error>(doc_id)
                    })
                })
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn pending_admission_rejection_selects_terminal_no_message() {
        let fixture = AdmissionFixture::new("pending-rejection").await;
        let agent_did = fixture.agent.did().to_owned();
        let create = fixture.local_self_create("rejected-request", "session", &agent_did, None);
        let doc_id = fixture.create_signed_request(create).await;

        super::terminalize_pending_request_rejection(
            fixture.node(),
            &doc_id,
            fixture.runtime.did(),
            &agent_did,
            fixture.agent.as_ref(),
            "request activates a skill outside its context allowlist",
            "test.admission_rejection",
        )
        .await
        .unwrap();

        let escaped_doc_id = crate::store::graphql::escape_graphql_string(&doc_id);
        let selected = fixture
            .node()
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{ lifecycle_state failure_reason terminal_output }} }}"#
            ))
            .await;
        assert!(!selected.has_errors(), "{:?}", selected.errors);
        let row = crate::store::graphql::first_row::<serde_json::Value>(&selected, "AgentRequest")
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
        fixture
            .db
            .node
            .shutdown()
            .await
            .expect("shut down test node");
    }

    #[tokio::test]
    async fn signed_input_cannot_expand_context_or_spoof_runtime_queue() {
        let fixture = AdmissionFixture::new("signed-input").await;
        let agent_did = fixture.agent.did().to_owned();
        let owner = fixture.runtime.did().to_owned();
        let context = format!("{agent_did}:context");
        fixture
            .user
            .transact_write("test.admission_context_allowlist", |txn| {
                let owner = &owner;
                let context = &context;
                Box::pin(async move {
                    let (_, mut record) =
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            crate::Collection::AgentContext,
                            owner,
                            context,
                        )
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("fixture AgentContext is missing"))?;
                    record["skill_ids"] = serde_json::json!(["allowed"]);
                    let skill = serde_json::json!({
                        "skill_id": "allowed", "node_did": owner, "name": "Allowed skill",
                        "instructions": "Admitted by the context allowlist.", "enabled": true,
                    });
                    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
                        crate::store::config_client::DesiredStateApplyDocument {
                            collection: crate::Collection::Skill,
                            add: skill.clone(),
                            update: skill,
                        },
                        crate::store::config_client::DesiredStateApplyDocument {
                            collection: crate::Collection::AgentContext,
                            add: record.clone(),
                            update: record,
                        },
                    ])?;
                    crate::documents::apply_desired_state_plan(txn, &plan).await
                })
            })
            .await
            .unwrap();
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
            let create = fixture.agent_self_create(
                &format!("input-{index}"),
                "session",
                "work",
                Some(input),
            );
            let doc_id = fixture.create_agent_signed_request(create).await;
            let request = super::load_request_for_admission_test(fixture.node(), &doc_id)
                .await
                .unwrap();
            let admitted = super::verify_fresh_local_self_request(
                fixture.node(),
                fixture.runtime.as_ref(),
                &request,
                &agent_did,
            )
            .await;
            assert_eq!(admitted.is_ok(), allowed, "case {index}: {admitted:?}");
        }
        fixture
            .db
            .node
            .shutdown()
            .await
            .expect("shut down test node");
    }

    #[tokio::test]
    async fn admission_rejection_projects_the_failed_turn_into_the_request_owned_session_only() {
        let temp = tempfile::tempdir().unwrap();
        let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap();
        let fixture = AdmissionFixture::new("admission-projection").await;
        let agent_did = fixture.agent.did().to_owned();
        let owner = crate::store::graphql::escape_graphql_string(fixture.runtime.did());
        fixture
            .ensure_agent_session("foreign-session", Some(foreign.did()))
            .await;

        let refusal_reason =
            "request admission denied: request activates a skill outside its context allowlist";
        for (request_id, session_id) in [
            ("refused-skill", "session"),
            ("foreign-scope", "foreign-session"),
        ] {
            let create = fixture.local_self_create(
                request_id,
                session_id,
                &agent_did,
                Some(serde_json::json!({"selected_skill_ids": ["unconfigured"]})),
            );
            let doc_id = fixture.create_signed_request(create).await;

            let request = super::load_request_for_admission_test(fixture.node(), &doc_id)
                .await
                .unwrap();
            assert!(
                super::verify_fresh_local_self_request(
                    fixture.node(),
                    fixture.runtime.as_ref(),
                    &request,
                    &agent_did,
                )
                .await
                .is_err(),
                "{request_id} must be denied by the context allowlist"
            );

            super::terminalize_pending_request_rejection(
                fixture.node(),
                &doc_id,
                fixture.runtime.did(),
                &agent_did,
                fixture.agent.as_ref(),
                refusal_reason,
                "test.admission_rejection_projection",
            )
            .await
            .unwrap();

            let selected = fixture
                .read_as_agent(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{owner}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::store::graphql::escape_graphql_string(session_id),
                    gents_protocol::session::AGENT_SESSION_FIELDS,
                ))
                .await;
            assert!(!selected.has_errors(), "{:?}", selected.errors);
            let rows = selected.data.as_ref().unwrap()["AgentSession"]
                .as_array()
                .cloned()
                .unwrap();
            assert_eq!(rows.len(), 1, "{request_id} must not add session rows");
            let session = gents_protocol::session::decode_session_row(&rows[0])
                .unwrap()
                .session;
            if session_id == "session" {
                assert_eq!(
                    session.requester_did.as_deref(),
                    Some(fixture.runtime.did())
                );
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
        fixture
            .db
            .node
            .shutdown()
            .await
            .expect("shut down test node");
    }

    async fn refused_pending_request_under_session(
        fixture: &AdmissionFixture,
        request_id: &str,
        session_id: &str,
        agent_did: &str,
    ) -> String {
        let create = fixture.local_self_create(
            request_id,
            session_id,
            agent_did,
            Some(serde_json::json!({"selected_skill_ids": ["unconfigured"]})),
        );
        fixture.create_signed_request(create).await
    }

    #[tokio::test]
    async fn refusal_survives_undecodable_session_rows_sharing_its_label() {
        let temp = tempfile::tempdir().unwrap();
        let foreign =
            KeyIdentity::load_or_create(temp.path().join("corrupt-foreign.key"), None).unwrap();
        let fixture = AdmissionFixture::new("corrupt-session-rows").await;
        let agent_did = fixture.agent.did().to_owned();
        let escaped_agent_did = crate::store::graphql::escape_graphql_string(&agent_did);
        let node_did = crate::store::graphql::escape_graphql_string(fixture.runtime.did());
        for (session_id, requester_did) in [
            ("corrupt-foreign-scope", Some(foreign.did())),
            ("corrupt-own-scope", Some(fixture.runtime.did())),
        ] {
            let requester = match requester_did {
                Some(did) => format!(
                    r#"requester_did: "{}","#,
                    crate::store::graphql::escape_graphql_string(did)
                ),
                None => "requester_did: null,".to_owned(),
            };
            fixture
                .write_as_agent(
                    format!(
                        r#"mutation {{ create_AgentSession(input: {{
                        session_id: "{}", node_did: "{node_did}", {requester} agent_did: "{escaped_agent_did}",
                        created_at: "not-an-rfc3339-timestamp"
                    }}) {{ _docID }} }}"#,
                        crate::store::graphql::escape_graphql_string(session_id),
                    ),
                    "AgentSession",
                )
                .await;

            let request_id = format!("refused-under-{session_id}");
            let doc_id = refused_pending_request_under_session(
                &fixture,
                &request_id,
                session_id,
                &agent_did,
            )
            .await;
            super::terminalize_pending_request_rejection(
                fixture.node(),
                &doc_id,
                fixture.runtime.did(),
                &agent_did,
                fixture.agent.as_ref(),
                "request admission denied: request activates a skill outside its context allowlist",
                "test.admission_rejection_corrupt_session_row",
            )
            .await
            .unwrap_or_else(|error| panic!("{session_id}: the refusal must persist: {error}"));

            let selected = fixture
                .node()
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                    crate::store::graphql::escape_graphql_string(&doc_id),
                ))
                .await;
            assert!(!selected.has_errors(), "{:?}", selected.errors);
            assert_eq!(
                selected.data.as_ref().unwrap()["AgentRequest"][0]["lifecycle_state"],
                "failed",
                "{session_id}: the refused request must not stay pending"
            );
            let sessions = fixture
                .read_as_agent(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::store::graphql::escape_graphql_string(session_id),
                    gents_protocol::session::AGENT_SESSION_FIELDS,
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
                gents_protocol::session::decode_session_row(&rows[0]).is_err(),
                "{session_id}: the refusal must leave the undecodable row untouched"
            );
        }
        fixture
            .db
            .node
            .shutdown()
            .await
            .expect("shut down test node");
    }

    #[tokio::test]
    async fn refusal_survives_unreadable_scoped_request_facts() {
        let fixture = AdmissionFixture::new("corrupt-facts").await;
        let agent_did = fixture.agent.did().to_owned();
        let escaped_agent_did = crate::store::graphql::escape_graphql_string(&agent_did);
        let node_did = crate::store::graphql::escape_graphql_string(fixture.runtime.did());
        for (session_id, sibling_request_id, sibling_created_at) in [
            ("facts-invalid-created-at", "sibling-invalid", "yesterday"),
            (
                "facts-duplicate-roots",
                "refused-dup-root",
                "2026-09-01T00:00:00Z",
            ),
        ] {
            fixture
                .ensure_agent_session(session_id, Some(fixture.runtime.did()))
                .await;
            let request_id = if session_id == "facts-duplicate-roots" {
                "refused-dup-root".to_owned()
            } else {
                format!("refused-{session_id}")
            };
            let doc_id = refused_pending_request_under_session(
                &fixture,
                &request_id,
                session_id,
                &agent_did,
            )
            .await;
            let sibling = fixture
                .write_as_runtime(
                    format!(
                        r#"mutation {{ create_AgentRequest(input: {{
                        request_id: "{}", purpose: "normal", node_did: "{node_did}",
                        requester_did: "{node_did}", agent_did: "{escaped_agent_did}", session_id: "{}",
                        content: "sibling turn", lifecycle_state: "pending",
                        created_at: "{}", retry_count: 0, max_retries: 3, request_hop: 0
                    }}) {{ _docID }} }}"#,
                        crate::store::graphql::escape_graphql_string(sibling_request_id),
                        crate::store::graphql::escape_graphql_string(session_id),
                        crate::store::graphql::escape_graphql_string(sibling_created_at),
                    ),
                    "AgentRequest",
                )
                .await;
            fixture.grant_working_agent(&sibling, &agent_did).await;

            super::terminalize_pending_request_rejection(
                fixture.node(),
                &doc_id,
                fixture.runtime.did(),
                &agent_did,
                fixture.agent.as_ref(),
                "request admission denied: request activates a skill outside its context allowlist",
                "test.admission_rejection_unreadable_facts",
            )
            .await
            .unwrap_or_else(|error| panic!("{session_id}: the refusal must persist: {error}"));

            let selected = fixture
                .node()
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                    crate::store::graphql::escape_graphql_string(&doc_id),
                ))
                .await;
            assert!(!selected.has_errors(), "{:?}", selected.errors);
            assert_eq!(
                selected.data.as_ref().unwrap()["AgentRequest"][0]["lifecycle_state"],
                "failed",
                "{session_id}: the refused request must not stay pending"
            );
            let sessions = fixture
                .read_as_agent(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::store::graphql::escape_graphql_string(session_id),
                    gents_protocol::session::AGENT_SESSION_FIELDS,
                ))
                .await;
            assert!(!sessions.has_errors(), "{:?}", sessions.errors);
            let rows = sessions.data.as_ref().unwrap()["AgentSession"]
                .as_array()
                .cloned()
                .unwrap();
            assert_eq!(rows.len(), 1, "{session_id}: one session row");
            let session = gents_protocol::session::decode_session_row(&rows[0])
                .unwrap()
                .session;
            assert!(
                session.observation.is_none(),
                "{session_id}: a projection the facts cannot order must leave the session \
                 untouched, got {:?}",
                session.observation
            );
        }
        fixture
            .db
            .node
            .shutdown()
            .await
            .expect("shut down test node");
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
                let fixture = AdmissionFixture::new("version-skew").await;
                let agent_did = fixture.agent.did().to_owned();
                let escaped_agent_did = crate::store::graphql::escape_graphql_string(&agent_did);
                let node_did = crate::store::graphql::escape_graphql_string(fixture.runtime.did());
                let doc_id = fixture
                    .write_as_runtime(
                        format!(
                            r#"mutation {{ create_AgentRequest(input: {{
                            request_id: "version-skewed", purpose: "normal", node_did: "{node_did}",
                            requester_did: "{node_did}", agent_did: "{escaped_agent_did}",
                            session_id: "skew-session", content: "run the disallowed skill",
                            lifecycle_state: "pending", created_at: "2026-09-01T00:00:00Z",
                            input: {{ future_field: "written by a newer runtime" }},
                            retry_count: 0, max_retries: 3, request_hop: 0
                        }}) {{ _docID }} }}"#
                        ),
                        "AgentRequest",
                    )
                    .await;
                fixture.grant_working_agent(&doc_id, &agent_did).await;

                super::terminalize_pending_request_rejection(
                    fixture.node(),
                    &doc_id,
                    fixture.runtime.did(),
                    &agent_did,
                    fixture.agent.as_ref(),
                    "request admission denied: request activates a skill outside its context allowlist",
                    "test.admission_rejection_version_skew",
                )
                .await
                .unwrap();

                let selected = fixture
                    .node()
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} AgentSession(filter: {{ session_id: {{ _eq: "skew-session" }} }}) {{ _docID }} }}"#,
                        crate::store::graphql::escape_graphql_string(&doc_id),
                    ))
                    .await;
                assert!(!selected.has_errors(), "{:?}", selected.errors);
                let data = selected.data.as_ref().unwrap();
                assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "failed");
                assert!(
                    data["AgentSession"].as_array().unwrap().is_empty(),
                    "an undecodable row mints no session"
                );
                fixture
                    .db
                    .node
                    .shutdown()
                    .await
                    .expect("shut down test node");
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
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        // A runtime-local-control receipt is issued and signed by the target Agent itself.
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "receipt-child",
            identity.did(),
            identity.did(),
            agent.did(),
            "receipt-session",
            "original continuation",
            "scheduled",
            "2020-01-01T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_local_control(agent.did(), "receipt-parent"),
        );
        create.caused_by_parent_request_id = Some("receipt-parent".into());
        create.caused_by_parent_request_doc_id = Some("receipt-parent-doc".into());
        create.valid_until = Some("2020-01-01T00:00:01Z".into());
        crate::sign_agent_request_create(agent.as_ref(), &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let query = format!("{{ AgentRequest {{ {} }} }}", super::SIGNED_REQUEST_FIELDS);
        let response = node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let mut row: gents_protocol::row::AgentRequestRow =
            crate::store::graphql::first_row(&response, "AgentRequest")
                .unwrap()
                .unwrap();
        row.lifecycle_state =
            Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed);
        row.deadline = Some("2020-01-01T00:00:01Z".into());
        crate::store::identity::request_receipt::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent",
            identity.did(),
        )
        .expect("terminal and expired original receipt remains authenticated");
        assert!(
            crate::store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                identity.did(),
                "different-parent",
                identity.did(),
            )
            .is_err()
        );
        let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap();
        assert!(
            crate::store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                foreign.did(),
                "receipt-parent",
                foreign.did(),
            )
            .is_err()
        );
        assert!(
            crate::store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                identity.did(),
                "receipt-parent",
                foreign.did(),
            )
            .is_err()
        );
        row.content = Some("forged continuation".into());
        assert!(
            crate::store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                identity.did(),
                "receipt-parent",
                identity.did(),
            )
            .is_err()
        );
        row.content = Some("original continuation".into());
        row.admission_signature = Some("not-a-valid-signature".into());
        assert!(
            crate::store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                identity.did(),
                "receipt-parent",
                identity.did(),
            )
            .is_err()
        );
        node.shutdown().await.expect("shut down test node");
    }

    #[tokio::test]
    async fn final_verifier_returns_the_exact_fresh_signed_snapshot() {
        let fixture = AdmissionFixture::new("final-verifier").await;
        let agent_did = fixture.agent.did().to_owned();
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let create =
            fixture.agent_self_create(&request_id, &session_id, "durable signed content", None);
        let doc_id = fixture.create_agent_signed_request(create).await;

        let row = super::load_signed_request(fixture.node().as_ref(), &doc_id)
            .await
            .unwrap();
        crate::store::identity::request_receipt::verify_request_receipt_signature(&row)
            .expect("original local-self immutable receipt signature");
        let mut queued = super::row_into_agent_request(row, &doc_id).unwrap();
        queued.content = "stale queued content".to_string();

        fixture
            .admin
            .write(
                "test.final_verifier.runtime_state",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{_docID: {{_eq:"{}"}}}}, input: {{execution_generation:"previous-claim",max_total_tokens:0}}) {{_docID}} }}"#,
                    crate::store::graphql::escape_graphql_string(&doc_id)
                ),
            )
            .await
            .unwrap();
        let (_authority_owner, authority) = enrollment_authority_channel();
        let verifier = AgentRequestAdmissionVerifier::new(
            fixture.node().clone(),
            fixture.runtime.clone(),
            authority,
        );
        let verified = verifier.verify_fresh(&queued, &agent_did).await.unwrap();
        assert_eq!(verified.content, "durable signed content");
        assert_ne!(verified.content, queued.content);
        assert_eq!(
            verified.execution_generation.as_deref(),
            Some("previous-claim")
        );
        assert_eq!(verified.max_total_tokens, Some(0));

        fixture
            .admin
            .write(
                "test.final_verifier.preclaim_deadline",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ deadline: "2099-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
                    crate::store::graphql::escape_graphql_string(&queued.doc_id),
                ),
            )
            .await
            .unwrap();
        let error = verifier
            .verify_fresh(&queued, &agent_did)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("caller-authored execution deadline"));
        fixture
            .db
            .node
            .shutdown()
            .await
            .expect("shut down test node");
    }

    #[tokio::test]
    async fn runtime_trigger_source_requires_current_trigger_and_target_policy() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        ensure_runtime_schemas(&node).await.unwrap();
        let response = node
            .execute(
                r#"mutation {
                    task: create_Task(input: {
                        task_id: "task-1", node_did: "did:key:target", agent_did: "agent-1",
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
