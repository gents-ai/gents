//! Final, fresh `AgentRequest` authority check. Watcher delivery, replication,
//! queue residence, and caller-authored lineage are not authority.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_admission::{
    project_agent_request_admission_disposition, validate_signing_fields,
    AgentRequestAdmissionDisposition, AgentRequestAdmissionKind, AgentRequestAdmissionObservation,
    AgentRequestAdmissionRecord, AgentRequestSigningFields, RequestPurpose,
    RuntimeInternalSourceKind,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::request::watcher::AgentRequest;
use crate::runtime::agent::p2p_reconcile::EnrollmentAuthorityHandle;
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum UnattendedAgentAdmissionError {
    #[error("unattended admission actor does not match the selected Agent")]
    ActorMismatch,
    #[error("selected Agent configuration is unavailable")]
    AgentUnavailable,
    #[error("selected Agent is disabled")]
    AgentDisabled,
    #[error("selected Agent cannot invoke this Agent")]
    InvokeDenied,
    #[error("selected Agent invoke authority is unavailable")]
    InvokeUnavailable,
    #[error("selected Agent observation failed: {0}")]
    Observation(#[source] anyhow::Error),
}

/// Observe the selected Agent's current native invoke authority in the fresh
/// admission transaction. The caller authenticates the transaction actor;
/// already admitted work continues under its own native effect checks.
pub(crate) async fn observe_unattended_agent_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    selected: &str,
) -> std::result::Result<(), UnattendedAgentAdmissionError> {
    use UnattendedAgentAdmissionError as Error;
    if txn.actor_did() != Some(selected) {
        return Err(Error::ActorMismatch);
    }
    let (doc, value) = crate::store::config_client::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Agent,
        owner,
        selected,
    )
    .await
    .map_err(Error::Observation)?
    .ok_or(Error::AgentUnavailable)?;
    let agent: crate::documents::Agent =
        serde_json::from_value(value).map_err(|error| Error::Observation(error.into()))?;
    if !agent.enabled {
        return Err(Error::AgentDisabled);
    }
    let actor = identity::Did::new(selected).map_err(|error| Error::Observation(error.into()))?;
    let decisions = txn
        .observe_document_access(
            &actor,
            "Agent",
            &[doc],
            defra_node::DocumentAccessMode::Named {
                permission: "invoke".into(),
                node_permission: acp::nac::NodePermission::DocumentUpdate,
            },
        )
        .await
        .map_err(|error| Error::Observation(error.into()))?;
    match decisions.as_slice() {
        [defra_node::DocumentAccessDecision::Allowed] => Ok(()),
        [defra_node::DocumentAccessDecision::Denied] => Err(Error::InvokeDenied),
        _ => Err(Error::InvokeUnavailable),
    }
}

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
            client_requester_scope_matches(node, &row, &admission).await?;
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
    _admission: &AgentRequestAdmissionRecord,
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
                        .and_then(crate::request::watcher::AgentRequest::try_from)
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
        let Some(session) = crate::session::load_agent_session_owner_in_txn(
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
            verify_auxiliary_request_shape(&row, &admission)?;
        }
        let local_agent_message = admission.kind == AgentRequestAdmissionKind::Client
            && row.caused_by_parent_tool_call_doc_id.is_some()
            && row
                .input
                .as_ref()
                .is_some_and(|input| input.session_creation.is_none());
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
                    client_requester_scope_matches(&self.node, &row, &admission).await?;
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

fn verify_auxiliary_request_shape(
    row: &AgentRequestRow,
    admission: &AgentRequestAdmissionRecord,
) -> AdmissionResult<()> {
    let target = required_row_string(row.agent_did.as_deref(), "agent_did")?;
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
) -> anyhow::Result<crate::request::lifecycle::RequestHopCapObservation> {
    Ok(crate::request::lifecycle::RequestHopCapObservation::of(
        crate::documents::load_node(node, node_did)
            .await?
            .map(|node_doc| node_doc.max_request_hop),
    ))
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
        .map_err(anyhow::Error::msg)?;
    let fields = row_signing_fields(row)?;
    validate_signing_fields(&fields)?;
    anyhow::ensure!(
        crate::store::identity::verify_did_signature(
            &admission.signer_did,
            &admission.signing_payload(&fields),
            &admission.signature,
        )?,
        "immutable request receipt signature is invalid"
    );
    Ok(())
}

pub(crate) fn signed_request_matches_create(
    row: &AgentRequestRow,
    expected: &gents_protocol::request_admission::AgentRequestCreate,
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
            && request.agent_did == parent.agent_did,
        "auxiliary receipt crosses its authenticated parent scope"
    );
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
    anyhow::ensure!(
        row.purpose == Some(RequestPurpose::Normal)
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
            && row.caused_by_parent_tool_call_doc_id.is_none(),
        "runtime local-control receipt does not match expected target/source binding"
    );
    Ok(())
}

fn row_admission(row: &AgentRequestRow) -> Result<AgentRequestAdmissionRecord> {
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
    .map_err(anyhow::Error::msg)?;
    if admission.kind == AgentRequestAdmissionKind::Client {
        anyhow::ensure!(
            row.session_doc_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
                && row.agent_doc_id.as_deref().is_some_and(|id| !id.is_empty()),
            "client request lacks signed physical session or agent identity"
        );
    }
    Ok(admission)
}

static DEFAULT_REQUEST_INPUT: std::sync::LazyLock<gents_protocol::request_input::RequestInput> =
    std::sync::LazyLock::new(Default::default);

fn row_signing_fields(row: &AgentRequestRow) -> Result<AgentRequestSigningFields<'_>> {
    let request_hop = row
        .request_hop
        .map(u32::try_from)
        .transpose()
        .context("AgentRequest request_hop must fit in u32")?
        .unwrap_or(0);
    Ok(AgentRequestSigningFields {
        request_id: &row.request_id,
        purpose: row.purpose.context("AgentRequest is missing purpose")?,
        node_did: row
            .node_did
            .as_deref()
            .context("AgentRequest is missing node_did")?,
        requester_did: row.requester_did.as_deref(),
        agent_did: row
            .agent_did
            .as_deref()
            .context("AgentRequest is missing agent_did")?,
        session_id: row
            .session_id
            .as_deref()
            .context("AgentRequest is missing session_id")?,
        session_doc_id: row.session_doc_id.as_deref(),
        agent_doc_id: row.agent_doc_id.as_deref(),
        retry_parent_request: row.retry_parent_request.as_deref(),
        retry_parent_request_doc_id: row.retry_parent_request_doc_id.as_deref(),
        retry_root_request: row.retry_root_request.as_deref(),
        retry_key: row.retry_key.as_deref(),
        content: row
            .content
            .as_deref()
            .context("AgentRequest is missing content")?,
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
            .context("AgentRequest is missing created_at")?,
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
    AgentRequest::try_from(row)
}

fn required_row_string<'a>(value: Option<&'a str>, field: &str) -> AdmissionResult<&'a str> {
    value.ok_or_else(|| {
        AgentRequestAdmissionError::denied(anyhow::anyhow!("AgentRequest is missing {field}"))
    })
}

macro_rules! signed_request_receipt_fields {
    () => {
        r#"
_docID request_id purpose node_did requester_did agent_did session_id session_doc_id agent_doc_id
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

    #[tokio::test]
    async fn pending_admission_rejection_selects_terminal_no_message() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(temp.path().join("rejection.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "rejected-request",
            identity.did(),
            identity.did(),
            agent.did(),
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
        let doc_id =
            crate::store::graphql::single_mutation_document(&created, "create_AgentRequest")
                .unwrap()
                .unwrap()["_docID"]
                .as_str()
                .unwrap()
                .to_owned();

        super::terminalize_pending_request_rejection(
            &node,
            &doc_id,
            identity.did(),
            agent.did(),
            agent.as_ref(),
            "request activates a skill outside its context allowlist",
            "test.admission_rejection",
        )
        .await
        .unwrap();

        let escaped_doc_id = crate::store::graphql::escape_graphql_string(&doc_id);
        let selected = node
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
    }

    #[tokio::test]
    async fn signed_input_cannot_expand_context_or_spoof_runtime_queue() {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("input.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let owner = crate::store::graphql::escape_graphql_string(identity.did());
        for mutation in [
            format!(
                r#"mutation {{ create_AgentContext(input: {{ context_id: "context", node_did: "{owner}", skill_ids: ["allowed"] }}) {{ _docID }} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{ agent_did: "agent", node_did: "{owner}", context_id: "context", inference_profile_id: "inference", enabled: true }}) {{ _docID }} }}"#
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
            let doc =
                crate::store::graphql::single_mutation_document(&result, "create_AgentRequest")
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
            defra_node::Node::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let agent_did = crate::store::graphql::escape_graphql_string(agent.did());
        let owner = crate::store::graphql::escape_graphql_string(identity.did());
        for mutation in [
            format!(
                r#"mutation {{ create_AgentContext(input: {{ context_id: "context", node_did: "{owner}", skill_ids: ["allowed"] }}) {{ _docID }} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{ agent_did: "{agent_did}", node_did: "{owner}", context_id: "context", inference_profile_id: "inference", enabled: true }}) {{ _docID }} }}"#
            ),
        ] {
            let result = node.execute(&mutation).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
        }
        crate::session::ensure_session_with_agent_did_and_requester_did(
            &node,
            "foreign-session",
            identity.did(),
            agent.did(),
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
                agent.did(),
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
            let doc_id =
                crate::store::graphql::single_mutation_document(&result, "create_AgentRequest")
                    .unwrap()
                    .unwrap()["_docID"]
                    .as_str()
                    .unwrap()
                    .to_owned();

            let request = super::load_request_for_admission_test(&node, &doc_id)
                .await
                .unwrap();
            assert!(
                super::verify_fresh_local_self_request(&node, &identity, &request, agent.did())
                    .await
                    .is_err(),
                "{request_id} must be denied by the context allowlist"
            );

            super::terminalize_pending_request_rejection(
                &node,
                &doc_id,
                identity.did(),
                agent.did(),
                agent.as_ref(),
                refusal_reason,
                "test.admission_rejection_projection",
            )
            .await
            .unwrap();

            let selected = node
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{owner}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::store::graphql::escape_graphql_string(session_id),
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
        node.shutdown().await.expect("shut down test node");
    }

    async fn refused_pending_request_under_session(
        node: &defra_node::Node,
        identity: &crate::store::identity::KeyIdentity,
        request_id: &str,
        session_id: &str,
        agent_did: &str,
    ) -> String {
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            agent_did,
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
        crate::store::graphql::single_mutation_document(&result, "create_AgentRequest")
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
            defra_node::Node::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let agent_did = crate::store::graphql::escape_graphql_string(agent.did());
        let node_did = crate::store::graphql::escape_graphql_string(identity.did());
        for (session_id, requester_did) in [
            ("corrupt-foreign-scope", Some(foreign.did())),
            ("corrupt-own-scope", Some(identity.did())),
        ] {
            let requester = match requester_did {
                Some(did) => format!(
                    r#"requester_did: "{}","#,
                    crate::store::graphql::escape_graphql_string(did)
                ),
                None => "requester_did: null,".to_owned(),
            };
            let seeded = node
                .execute(&format!(
                    r#"mutation {{ create_AgentSession(input: {{
                        session_id: "{}", node_did: "{node_did}", {requester} agent_did: "{agent_did}",
                        created_at: "not-an-rfc3339-timestamp"
                    }}) {{ _docID }} }}"#,
                    crate::store::graphql::escape_graphql_string(session_id),
                ))
                .await;
            assert!(!seeded.has_errors(), "{session_id}: {:?}", seeded.errors);

            let request_id = format!("refused-under-{session_id}");
            let doc_id = refused_pending_request_under_session(
                &node,
                &identity,
                &request_id,
                session_id,
                agent.did(),
            )
            .await;
            super::terminalize_pending_request_rejection(
                &node,
                &doc_id,
                identity.did(),
                agent.did(),
                agent.as_ref(),
                "request admission denied: request activates a skill outside its context allowlist",
                "test.admission_rejection_corrupt_session_row",
            )
            .await
            .unwrap_or_else(|error| panic!("{session_id}: the refusal must persist: {error}"));

            let selected = node
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
            let sessions = node
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::store::graphql::escape_graphql_string(session_id),
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
        node.shutdown().await.expect("shut down test node");
    }

    #[tokio::test]
    async fn refusal_survives_unreadable_scoped_request_facts() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(temp.path().join("corrupt-facts.key"), None).unwrap();
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let agent_did = crate::store::graphql::escape_graphql_string(agent.did());
        let node_did = crate::store::graphql::escape_graphql_string(identity.did());
        for (session_id, sibling_request_id, sibling_created_at) in [
            ("facts-invalid-created-at", "sibling-invalid", "yesterday"),
            (
                "facts-duplicate-roots",
                "refused-dup-root",
                "2026-09-01T00:00:00Z",
            ),
        ] {
            crate::session::ensure_session_with_agent_did_and_requester_did(
                &node,
                session_id,
                identity.did(),
                agent.did(),
                Some(identity.did()),
            )
            .await
            .unwrap();
            let request_id = if session_id == "facts-duplicate-roots" {
                "refused-dup-root".to_owned()
            } else {
                format!("refused-{session_id}")
            };
            let doc_id = refused_pending_request_under_session(
                &node,
                &identity,
                &request_id,
                session_id,
                agent.did(),
            )
            .await;
            let sibling = node
                .execute(&format!(
                    r#"mutation {{ create_AgentRequest(input: {{
                        request_id: "{}", purpose: "normal", node_did: "{node_did}",
                        requester_did: "{node_did}", agent_did: "{agent_did}", session_id: "{}",
                        content: "sibling turn", lifecycle_state: "pending",
                        created_at: "{}", retry_count: 0, max_retries: 3, request_hop: 0
                    }}) {{ _docID }} }}"#,
                    crate::store::graphql::escape_graphql_string(sibling_request_id),
                    crate::store::graphql::escape_graphql_string(session_id),
                    crate::store::graphql::escape_graphql_string(sibling_created_at),
                ))
                .await;
            assert!(!sibling.has_errors(), "{session_id}: {:?}", sibling.errors);

            super::terminalize_pending_request_rejection(
                &node,
                &doc_id,
                identity.did(),
                agent.did(),
                agent.as_ref(),
                "request admission denied: request activates a skill outside its context allowlist",
                "test.admission_rejection_unreadable_facts",
            )
            .await
            .unwrap_or_else(|error| panic!("{session_id}: the refusal must persist: {error}"));

            let selected = node
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
            let sessions = node
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    crate::store::graphql::escape_graphql_string(session_id),
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
        node.shutdown().await.expect("shut down test node");
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
                let node = std::sync::Arc::new(defra_node::Node::builder().with_node_identity_did(identity.did()).build().await.unwrap());
                ensure_runtime_schemas(&node).await.unwrap();
                let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
                let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
                let agent_did = crate::store::graphql::escape_graphql_string(agent.did());
                let node_did = crate::store::graphql::escape_graphql_string(identity.did());
                let created = node
                    .execute(&format!(
                        r#"mutation {{ create_AgentRequest(input: {{
                            request_id: "version-skewed", purpose: "normal", node_did: "{node_did}",
                            requester_did: "{node_did}", agent_did: "{agent_did}",
                            session_id: "skew-session", content: "run the disallowed skill",
                            lifecycle_state: "pending", created_at: "2026-09-01T00:00:00Z",
                            input: {{ future_field: "written by a newer runtime" }},
                            retry_count: 0, max_retries: 3, request_hop: 0
                        }}) {{ _docID }} }}"#
                    ))
                    .await;
                assert!(!created.has_errors(), "{:?}", created.errors);
                let doc_id = crate::store::graphql::single_mutation_document(&created, "create_AgentRequest")
                    .unwrap()
                    .unwrap()["_docID"]
                    .as_str()
                    .unwrap()
                    .to_owned();

                super::terminalize_pending_request_rejection(
                    &node,
                    &doc_id,
                    identity.did(),
                    agent.did(),
                    agent.as_ref(),
                    "request admission denied: request activates a skill outside its context allowlist",
                    "test.admission_rejection_version_skew",
                )
                .await
                .unwrap();

                let selected = node
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
                node.shutdown().await.expect("shut down test node");
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
            defra_node::Node::builder()
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
            crate::store::graphql::first_row(&response, "AgentRequest")
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
        node.shutdown().await.expect("shut down test node");
    }

    #[tokio::test]
    async fn final_verifier_returns_the_exact_fresh_signed_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap());
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(node.as_ref()).await.unwrap();

        let owner = crate::store::graphql::escape_graphql_string(identity.did());
        let seeded = node.execute(&format!(r#"mutation {{ create_Agent(input: {{agent_did:"agent-1",node_did:"{owner}",inference_profile_id:"inference",enabled:true}}) {{_docID}} }}"#)).await;
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
            crate::store::graphql::escape_graphql_string(&doc_id))).await;
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
                crate::store::graphql::escape_graphql_string(&queued.doc_id),
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
