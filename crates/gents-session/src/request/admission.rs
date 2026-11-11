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

use gents_p2p::p2p_reconcile::EnrollmentAuthorityHandle;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::request_receipt::{
    row_admission, row_signing_fields, verify_auxiliary_request_shape,
};
use gents_store::identity::NodeIdentity;

#[derive(Debug)]
pub enum AgentRequestAdmissionError {
    Denied(anyhow::Error),
    Unavailable(anyhow::Error),
}

impl AgentRequestAdmissionError {
    pub(crate) fn denied(error: impl Into<anyhow::Error>) -> Self {
        Self::Denied(error.into())
    }

    pub(crate) fn unavailable(error: impl Into<anyhow::Error>) -> Self {
        Self::Unavailable(error.into())
    }

    pub const fn is_denied(&self) -> bool {
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

pub(crate) type AdmissionResult<T> = std::result::Result<T, AgentRequestAdmissionError>;

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
pub enum PendingRejectionOutcome {
    Terminalized,
    NotMatched,
}

#[derive(Debug, thiserror::Error)]
pub enum PendingRejectionError {
    #[error("invalid selected agent identity: {0}")]
    InvalidAgent(#[source] anyhow::Error),
    #[error("pending request rejection failed: {0}")]
    Transaction(#[source] anyhow::Error),
}

pub async fn terminalize_pending_request_rejection(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    node_did: &str,
    agent_did: &str,
    agent_identity: &dyn gents_store::identity::NodeIdentity,
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
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        actor,
        agent_identity,
    )
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
        gents_store::config_client::IdempotentTransactionRetry::Standard,
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
                let rows = gents_store::graphql::mutation_documents(&response, "update_AgentRequest")?
                    .context("pending rejection mutation omitted its result")?;
                anyhow::ensure!(rows.len() <= 1, "pending rejection updated multiple requests");
                for row in rows {
                    crate::request::workspace_binding::release_terminal_writer_binding(txn, row).await?;
                    if let Some(request_id) = row["request_id"].as_str() {
                        crate::request::hooks::points::request_terminal(
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
    let identity = gents_store::identity::RegisteredIdentity::from_registered_did(
        request.agent_did.clone(),
        None,
    )
    .context("load registered target agent identity for AgentRequest authoring")?;
    sign_agent_request_create(&identity, request).await
}

/// Immutable signed admission retained until the native claim transaction.
#[derive(Debug, Clone)]
pub struct ClientClaimAdmission {
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
        txn: &gents_store::config_client::ConfigApplyTxn<'_>,
        request: &AgentRequest,
    ) -> Result<ClientClaimAuthorization> {
        use gents_protocol::document_query::{
            fields::{
                agent as agent_fields, agent_request as request_fields,
                agent_session as session_fields,
            },
            Filter, Query, Selection,
        };
        use ClientClaimAuthorization::{Allowed, Denied, Unavailable};
        let rendered = Query::select(
            Selection::owned(SIGNED_REQUEST_FIELDS).field(request_fields::RELATIONSHIPS_PENDING),
        )
        .filter(Filter::eq(request_fields::DOC_ID, &request.doc_id))
        .limit(2)
        .render();
        let response = txn.run(&rendered).await?;
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
        let rendered =
            Query::select(Selection::doc_id().field(session_fields::RELATIONSHIPS_PENDING))
                .filter(Filter::eq(session_fields::DOC_ID, session_doc))
                .limit(2)
                .render();
        let response = txn.run(&rendered).await?;
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
            gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
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
        let rendered = Query::select(
            Selection::doc_id()
                .field(agent_fields::NODE_DID)
                .field(agent_fields::AGENT_DID)
                .field(agent_fields::ENABLED),
        )
        .filter(Filter::eq(agent_fields::DOC_ID, agent_doc))
        .limit(2)
        .render();
        let target = txn.run(&rendered).await?;
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
pub struct AgentRequestAdmissionVerifier {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    enrollment: EnrollmentAuthorityHandle,
}

impl AgentRequestAdmissionVerifier {
    pub fn new(
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

    pub async fn verify_fresh_for_claim_with_identity(
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
        // The existing admission projection accepts the owner-installed deadline
        // only after reconstructing its exact, immutable claim version.
        observation.pending_deadline_absent =
            if row.lifecycle_state == Some(RequestLifecycleState::WaitingForBackend) {
                let actor = identity::Did::new(target_agent_did)
                    .map_err(AgentRequestAdmissionError::denied)?;
                let access =
                    gents_store::config_client::ConfigAccess::local_as(self.node.clone(), actor);
                verify_resumed_claim_version(&access, &row).await?
            } else {
                row.deadline.is_none()
            };
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
                        &gents_store::config_client::ConfigAccess::local_as(
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
                            &gents_store::config_client::ConfigAccess::local_as(
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
        == Some(gents_loop::tool_policy::CommandExecutionMode::ArtifactWrite);
    if let Some(agent_identity) = agent_identity {
        crate::workspace::validate_request_workspace_input_with_identity(
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
        crate::workspace::validate_unbound_request_workspace_input(&request, artifact_requested)
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
            gents_store::config_client::ConfigAccess::transact_local_readonly(
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
    }
    crate::request::hooks::points::request_admit_verify(node, row, agent).await
}

pub(crate) fn validate_authored_content(
    input: &gents_protocol::request_input::RequestInput,
) -> AdmissionResult<()> {
    deny_if(
        input
            .captured_context
            .iter()
            .all(|context| context.is_well_formed()),
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

pub async fn max_request_hop(
    node: &EmbeddedNode,
    node_did: &str,
) -> anyhow::Result<gents_protocol::request_lifecycle::RequestHopCapObservation> {
    Ok(
        gents_protocol::request_lifecycle::RequestHopCapObservation::of(
            gents_documents::load_node(node, node_did)
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

pub(crate) async fn load_exact_parent_request(
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

pub async fn load_request_context(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
) -> AdmissionResult<(
    Option<gents_documents::AgentContext>,
    Option<gents_documents::Tools>,
    Vec<gents_documents::AgentTargetDocument>,
)> {
    use gents_store::config_client::ConfigAccess;
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
pub async fn load_request_context_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    agent_did: &str,
) -> anyhow::Result<(
    Option<gents_documents::AgentContext>,
    Option<gents_documents::Tools>,
    Vec<gents_documents::AgentTargetDocument>,
)> {
    use gents_documents::read_desired_state_document_in_txn as read;
    use gents_documents::{Agent, AgentContext, AgentTargetDocument, Tools};
    use gents_store::collection::Collection;
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
        source: gents_documents::TriggerSource,
        enabled: bool,
    }
    #[derive(Deserialize)]
    struct TaskRow {
        agent_did: String,
        enabled: bool,
    }
    use gents_protocol::document_query::{
        fields::{task as task_fields, trigger as trigger_fields},
        Filter, Query, Selection,
    };
    let doc = required_row_string(trigger_doc_id, "trigger document ID")?;
    let trigger_query = Query::select(
        Selection::new()
            .field(trigger_fields::TRIGGER_ID)
            .field(trigger_fields::NODE_DID)
            .field(trigger_fields::TASK_ID)
            .field(trigger_fields::SOURCE)
            .field(trigger_fields::ENABLED),
    )
    .filter(Filter::eq(trigger_fields::DOC_ID, doc))
    .limit(1)
    .render();
    let response =
        gents_store::graphql::read_rendered(node, &trigger_query, "reload runtime trigger")
            .await
            .map_err(AgentRequestAdmissionError::unavailable)?;
    let triggers: Vec<TriggerRow> = gents_store::graphql::rows(&response, "Trigger")
        .map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        triggers.len() == 1,
        "runtime trigger no longer exists; it was removed before this fire ran",
    )?;
    let trigger = &triggers[0];
    deny_if(
        trigger.enabled
            && trigger.trigger_id == trigger_id
            && trigger.node_did == node_did
            && matches!(
                (&trigger.source, kind),
                (gents_documents::TriggerSource::Event { .. }, "event")
                    | (gents_documents::TriggerSource::Schedule { .. }, "schedule")
            ),
        "runtime trigger physical source, node, kind, or availability changed",
    )?;
    let task_query = Query::select(
        Selection::new()
            .field(task_fields::AGENT_DID)
            .field(task_fields::ENABLED),
    )
    .filter(Filter::eq(task_fields::TASK_ID, &trigger.task_id))
    .filter(Filter::eq(task_fields::NODE_DID, node_did))
    .limit(2)
    .render();
    let response =
        gents_store::graphql::read_rendered(node, &task_query, "reload runtime trigger task")
            .await
            .map_err(AgentRequestAdmissionError::unavailable)?;
    let tasks: Vec<TaskRow> = gents_store::graphql::rows(&response, "Task")
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

pub(crate) fn required_row_string<'a>(
    value: Option<&'a str>,
    field: &str,
) -> AdmissionResult<&'a str> {
    value.ok_or_else(|| {
        AgentRequestAdmissionError::denied(anyhow::anyhow!("AgentRequest is missing {field}"))
    })
}

async fn load_signed_request_as(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
    actor: &str,
) -> AdmissionResult<AgentRequestRow> {
    use gents_protocol::document_query::{
        fields::agent_request as request_fields, Filter, Query, Selection,
    };
    let actor = identity::Did::new(actor).map_err(AgentRequestAdmissionError::denied)?;
    let query = Query::select(Selection::owned(SIGNED_REQUEST_FIELDS))
        .filter(Filter::eq(request_fields::DOC_ID, doc_id))
        .limit(1)
        .render();
    let response = gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(actor),
        "request.admission.read_as_agent",
        |txn| {
            let query = &query;
            Box::pin(async move { txn.run(query).await })
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
    use gents_protocol::document_query::{
        fields::agent_request as request_fields, Filter, Query, Selection,
    };
    let query = Query::select(Selection::owned(SIGNED_REQUEST_FIELDS))
        .filter(Filter::eq(request_fields::DOC_ID, doc_id))
        .limit(1)
        .render();
    let response =
        gents_store::graphql::read_rendered(node, &query, "reload AgentRequest admission row")
            .await
            .map_err(AgentRequestAdmissionError::unavailable)?;
    gents_store::graphql::first_row(&response, "AgentRequest")
        .map_err(AgentRequestAdmissionError::denied)?
        .ok_or_else(|| {
            AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
                "AgentRequest disappeared before admission"
            ))
        })
}

#[derive(Debug)]
struct ResumeClaimFacts {
    waiting: bool,
    version_present: bool,
    claimed: bool,
    same_identity: bool,
    same_deadline: bool,
    same_backend: bool,
}

fn resume_claim_valid(facts: &ResumeClaimFacts) -> bool {
    facts.waiting
        && facts.version_present
        && facts.claimed
        && facts.same_identity
        && facts.same_deadline
        && facts.same_backend
}

/// Reads as the Agent that parked the request: it owns both the wait record
/// and the claim version being reconstructed.
pub(crate) async fn verify_resumed_claim_version(
    access: &gents_store::config_client::ConfigAccess,
    current: &AgentRequestRow,
) -> AdmissionResult<bool> {
    let Some(doc) = current.doc_id.as_deref() else {
        return Ok(false);
    };
    let doc_gql = escape_graphql_string(doc);
    let wait = access
        .execute(&format!(r#"{{ AgentRequestBackendWait(filter: {{ request_doc_id: {{ _eq: "{doc_gql}" }} }}, limit: 1) {{ node_did claim_cid }} }}"#))
        .await
        .map_err(AgentRequestAdmissionError::unavailable)?;
    let Some(wait) = wait.pointer("/data/AgentRequestBackendWait/0") else {
        return Ok(false);
    };
    if wait.get("node_did").and_then(serde_json::Value::as_str) != current.node_did.as_deref() {
        return Ok(false);
    }
    let Some(cid) = wait
        .get("claim_cid")
        .and_then(serde_json::Value::as_str)
        .filter(|cid| !cid.trim().is_empty())
    else {
        return Ok(false);
    };
    let cid_gql = escape_graphql_string(cid);
    let response = access
        .execute(&format!(r#"query {{ AgentRequest(cid: ["{cid_gql}"], docID: "{doc_gql}", showDeleted: true) {{ {SIGNED_REQUEST_FIELDS} }} }}"#))
        .await
        .map_err(AgentRequestAdmissionError::unavailable)?;
    let versions: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .map_err(AgentRequestAdmissionError::denied)?;
    if versions.is_empty() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "original request claim version is unavailable"
        )));
    }
    if versions.len() != 1 {
        return Ok(false);
    }
    let claimed = &versions[0];
    let current_admission = row_admission(current).map_err(AgentRequestAdmissionError::denied)?;
    let claimed_admission = row_admission(claimed).map_err(AgentRequestAdmissionError::denied)?;
    let current_fields = row_signing_fields(current).map_err(AgentRequestAdmissionError::denied)?;
    let claimed_fields = row_signing_fields(claimed).map_err(AgentRequestAdmissionError::denied)?;
    let facts = ResumeClaimFacts {
        waiting: current.lifecycle_state == Some(RequestLifecycleState::WaitingForBackend),
        version_present: claimed.doc_id.as_deref() == Some(doc),
        claimed: claimed.lifecycle_state == Some(RequestLifecycleState::Claimed)
            && claimed
                .execution_generation
                .as_deref()
                .is_some_and(|generation| !generation.trim().is_empty()),
        same_identity: current_admission.signature == claimed_admission.signature
            && current_admission.signing_payload(&current_fields)
                == claimed_admission.signing_payload(&claimed_fields)
            && current.max_total_tokens == claimed.max_total_tokens
            && current.execution_lease_secs == claimed.execution_lease_secs
            && current.background_completion_input_through_sequence
                == claimed.background_completion_input_through_sequence
            && current.background_completion_notification_keys_json
                == claimed.background_completion_notification_keys_json,
        same_deadline: current
            .deadline
            .as_deref()
            .is_some_and(|deadline| !deadline.trim().is_empty())
            && current.deadline == claimed.deadline,
        same_backend: current
            .backend_id
            .as_deref()
            .is_some_and(|backend| !backend.trim().is_empty())
            && current.backend_id == claimed.backend_id,
    };
    Ok(resume_claim_valid(&facts))
}

#[cfg(test)]
#[test]
fn resumed_claim_admission_matches_the_generated_model() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().backend_resume_admission_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let facts = ResumeClaimFacts {
            waiting: case.waiting,
            version_present: case.version_present,
            claimed: case.claimed,
            same_identity: case.same_identity,
            same_deadline: case.same_deadline,
            same_backend: case.same_backend,
        };
        assert_eq!(resume_claim_valid(&facts), case.admitted, "{case:?}");
    }
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
pub async fn load_request_for_admission_test_as(
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
    use gents_p2p::p2p_reconcile::enrollment_authority_channel;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    use gents_store::identity::{KeyIdentity, NodeIdentity};
    use gents_store::schema::ensure_runtime_schemas;

    #[test]
    fn generated_client_claim_queue_candidates_match_the_model() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().request_input_cases;
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
        let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let other_binding = gents_store::identity::provision_agent_identity(&other_root).unwrap();
        let other_agent =
            gents_store::identity::resolve_agent_identity(&other_root, &other_binding).unwrap();
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(root.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        gents_store::identity::GrantSet::AgentBaseline
            .grant(
                &gents_store::config_client::ConfigAccess::local_as(
                    node.clone(),
                    identity::Did::new(root.did()).unwrap(),
                ),
                &identity::Did::new(agent.did()).unwrap(),
            )
            .await
            .unwrap();
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

    #[tokio::test]
    async fn runtime_receipt_authenticates_terminal_expired_child_and_rejects_forgery() {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("receipt.key"), None).unwrap();
        let binding = gents_store::identity::provision_agent_identity(&identity).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(&identity, &binding).unwrap();
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
        crate::request::admission::sign_agent_request_create(agent.as_ref(), &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let query = format!("{{ AgentRequest {{ {} }} }}", super::SIGNED_REQUEST_FIELDS);
        let response = node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let mut row: gents_protocol::row::AgentRequestRow =
            gents_store::graphql::first_row(&response, "AgentRequest")
                .unwrap()
                .unwrap();
        row.lifecycle_state =
            Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed);
        row.deadline = Some("2020-01-01T00:00:01Z".into());
        gents_store::identity::request_receipt::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent",
            identity.did(),
        )
        .expect("terminal and expired original receipt remains authenticated");
        assert!(
            gents_store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                identity.did(),
                "different-parent",
                identity.did(),
            )
            .is_err()
        );
        let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap();
        assert!(
            gents_store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                foreign.did(),
                "receipt-parent",
                foreign.did(),
            )
            .is_err()
        );
        assert!(
            gents_store::identity::request_receipt::verify_runtime_local_control_receipt(
                &row,
                identity.did(),
                "receipt-parent",
                foreign.did(),
            )
            .is_err()
        );
        row.content = Some("forged continuation".into());
        assert!(
            gents_store::identity::request_receipt::verify_runtime_local_control_receipt(
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
            gents_store::identity::request_receipt::verify_runtime_local_control_receipt(
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
    async fn runtime_trigger_source_requires_current_trigger_and_target_policy() {
        let node = std::sync::Arc::new(
            gents_store::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
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

#[cfg(test)]
mod auxiliary_repair_tests;

#[cfg(test)]
mod auxiliary_binding_contract_tests;
