//! Final, fresh `AgentRequest` authority check.
//!
//! The daemon calls this immediately before the claim transaction.  Watcher
//! delivery, replication, queue residence, and caller-authored lineage are not
//! authority.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;
use gents_protocol::request_admission::{
    project_agent_request_admission_disposition, validate_signing_fields,
    AgentRequestAdmissionDisposition, AgentRequestAdmissionKind, AgentRequestAdmissionObservation,
    AgentRequestAdmissionRecord, AgentRequestSigningFields, RuntimeInternalSourceKind,
};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::agent::p2p_reconcile::{EnrollmentAuthorityHandle, PeerAdmissionAuthority};
use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;
use crate::watcher::AgentRequest;

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

/// Runtime-owned final claim projection. The conformance suite calls this same
/// seam so the Lean-generated admission matrix fences the decision used by the
/// durable verifier rather than only a protocol-local copy.
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
        requester_matches_issuer: false,
        current_approval: false,
        exact_generation: false,
        authorization_fresh: false,
        runtime_evidence_present: false,
        runtime_source_kind,
        target_runtime_attestation_valid: false,
        source_binding_current: false,
        trigger_config_document_binding_current: false,
        source_document_binding_current: false,
        source_tool_call_binding_current: false,
        target_policy_allows: false,
        bridge_author_binding_current: false,
        bridge_author_authorization_fresh: false,
        target_cross_deployment_policy_allows: false,
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

/// Authenticate a runtime-authored local-self row before an in-process caller
/// claims it. This is the one-shot counterpart of the daemon's final boundary:
/// possession of the target key authors the request, while a fresh durable
/// reload proves the row still matches that signature before claim.
pub(crate) async fn verify_fresh_local_self_request(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    request: &AgentRequest,
    target_behavior_id: &str,
) -> AdmissionResult<AgentRequest> {
    let row = load_signed_request(node, &request.doc_id).await?;
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
        && row.agent_did.as_deref() == Some(request.agent_did.as_str())
        && row.behavior_id.as_deref() == Some(target_behavior_id)
        && validate_signing_fields(&signing_fields).is_ok();
    observation.branch_fields_exact =
        admission.validate_canonical_fields().is_ok() && admission.validate_branch_fields().is_ok();
    observation.pending_deadline_absent = row.deadline.is_none();
    observation.signer_matches_requester =
        row.requester_did.as_deref() == Some(admission.signer_did.as_str());
    observation.requester_matches_target = row.requester_did.as_deref() == row.agent_did.as_deref();
    require_admitted_observation(observation, None)?;
    row_into_agent_request(row, &request.doc_id).map_err(AgentRequestAdmissionError::denied)
}

pub async fn sign_agent_request_create(
    identity: &dyn AgentIdentity,
    request: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    validate_signing_fields(&request.signing_fields())?;
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

/// Persist a fail-closed rejection before claim. This transition is shared by
/// hostile-row ingest and final authority verification; ordinary lifecycle
/// failure starts only after a successful claim.
pub(crate) async fn terminalize_pending_request_rejection(
    node: &EmbeddedNode,
    doc_id: &str,
    agent_did: &str,
    reason: &str,
    operation: &str,
) -> Result<()> {
    let doc_id = escape_graphql_string(doc_id);
    let agent_did = escape_graphql_string(agent_did);
    let failure_reason = escape_graphql_string(reason);
    let terminalized_at = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    lifecycle_state: {{ _eq: "pending" }}
                }},
                input: {{
                    lifecycle_state: "failed",
                    failure_reason: "{failure_reason}",
                    terminalized_at: "{terminalized_at}",
                    terminal_redrive_attempts: 0
                }}
            ) {{ _docID }}
        }}"#
    );
    crate::retry::execute_graphql_with_terminal_persistence_retry(node, &mutation, operation)
        .await
        .map(|_| ())
}

/// Sign a target-runtime-authored request with the already-registered runtime
/// principal. Runtime startup and initialized-home loaders register this exact
/// identity before any request authoring path becomes available.
pub async fn sign_agent_request_create_as_registered_target(
    request: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let identity =
        crate::identity::RegisteredIdentity::from_registered_did(request.agent_did.clone(), None)
            .context("load registered target runtime identity for AgentRequest authoring")?;
    sign_agent_request_create(&identity, request).await
}

#[derive(Clone)]
pub(crate) struct AgentRequestAdmissionVerifier {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    enrollment: EnrollmentAuthorityHandle,
    peer_admission: Arc<dyn PeerAdmissionAuthority>,
}

impl AgentRequestAdmissionVerifier {
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        identity: Arc<dyn AgentIdentity>,
        enrollment: EnrollmentAuthorityHandle,
    ) -> Self {
        Self {
            node,
            identity,
            enrollment: enrollment.clone(),
            peer_admission: Arc::new(enrollment),
        }
    }

    /// Reload and authenticate the exact durable row. This is the final
    /// linearization point before `claim_with_identity`.
    pub(crate) async fn verify_fresh(
        &self,
        request: &AgentRequest,
        target_behavior_id: &str,
    ) -> AdmissionResult<AgentRequest> {
        self.verify_fresh_with_observation(request, target_behavior_id, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn verify_fresh_at(
        &self,
        request: &AgentRequest,
        target_behavior_id: &str,
        observed_at: chrono::DateTime<Utc>,
    ) -> AdmissionResult<AgentRequest> {
        self.verify_fresh_with_observation(request, target_behavior_id, Some(observed_at))
            .await
    }

    async fn verify_fresh_with_observation(
        &self,
        request: &AgentRequest,
        target_behavior_id: &str,
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
            && row.agent_did.as_deref() == Some(request.agent_did.as_str())
            && row.behavior_id.as_deref() == Some(target_behavior_id)
            && validate_signing_fields(&signing_fields).is_ok();
        observation.branch_fields_exact = admission.validate_canonical_fields().is_ok()
            && admission.validate_branch_fields().is_ok();
        observation.pending_deadline_absent = row.deadline.is_none();
        if !observation.signature_valid
            || !observation.signed_fields_match
            || !observation.branch_fields_exact
            || !observation.pending_deadline_absent
        {
            require_admitted_observation(observation, None)?;
            unreachable!("negative common admission evidence cannot be admitted");
        }
        let mut denied = None;
        match admission.kind {
            AgentRequestAdmissionKind::LocalSelf => {
                observation.signer_matches_requester =
                    row.requester_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.agent_did.as_deref();
            }
            AgentRequestAdmissionKind::Enrollment => {
                let requester = nonempty(row.requester_did.as_deref());
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
                        // Production always samples after the async authority
                        // reload. Tests may inject this final observation only.
                        let observed_at = test_observed_at.unwrap_or_else(Utc::now);
                        observation.current_approval =
                            row.agent_did.as_deref() == Some(current.owner_agent.as_str());
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
                observation.requester_matches_issuer = row.requester_did.as_deref() == issuer;
                observation.signer_matches_target =
                    row.agent_did.as_deref() == Some(admission.signer_did.as_str());
                observation.requester_matches_target =
                    row.requester_did.as_deref() == row.agent_did.as_deref();
                observation.target_runtime_attestation_valid = issuer == row.agent_did.as_deref();
                if !observation.runtime_evidence_present
                    || !observation.signer_matches_issuer
                    || !observation.requester_matches_issuer
                    || !observation.signer_matches_target
                    || !observation.requester_matches_target
                    || !observation.target_runtime_attestation_valid
                {
                    require_admitted_observation(observation, None)?;
                    unreachable!("invalid runtime attestation cannot be admitted");
                }
                if let (Some(source), Some(source_kind)) = (source, admission.runtime_source_kind) {
                    match verify_runtime_source_binding(
                        self.node.as_ref(),
                        self.peer_admission.as_ref(),
                        &row,
                        source,
                        source_kind,
                        admission.runtime_bridge_author_did.as_deref(),
                        target_behavior_id,
                    )
                    .await
                    {
                        Ok(()) => {
                            observation.source_binding_current = true;
                            match source_kind {
                                RuntimeInternalSourceKind::LocalChild => {
                                    observation.source_document_binding_current = true;
                                    observation.source_tool_call_binding_current = true;
                                    observation.target_policy_allows = true;
                                }
                                RuntimeInternalSourceKind::CrossDeploymentChild => {
                                    observation.source_tool_call_binding_current = true;
                                    observation.bridge_author_binding_current = true;
                                    observation.bridge_author_authorization_fresh = true;
                                    observation.target_cross_deployment_policy_allows = true;
                                }
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
                            return Err(error)
                        }
                    }
                }
            }
        }
        require_admitted_observation(observation, denied)?;
        row_into_agent_request(row, &request.doc_id).map_err(AgentRequestAdmissionError::denied)
    }
}

async fn verify_runtime_source_binding(
    node: &EmbeddedNode,
    peer_admission: &dyn PeerAdmissionAuthority,
    row: &AgentRequestRow,
    source: &str,
    source_kind: RuntimeInternalSourceKind,
    bridge_author_did: Option<&str>,
    target_behavior_id: &str,
) -> AdmissionResult<()> {
    match source_kind {
        RuntimeInternalSourceKind::LocalChild => {
            deny_if(
                bridge_author_did.is_none()
                    && row.caused_by_parent_request_id.as_deref() == Some(source),
                "local-child runtime source branch is mixed or incoherent",
            )?;
            let parent_doc_id =
                row.caused_by_parent_request_doc_id
                    .as_deref()
                    .ok_or_else(|| {
                        AgentRequestAdmissionError::denied(anyhow::anyhow!(
                            "runtime-internal parent request document binding is absent"
                        ))
                    })?;
            let parent = load_exact_parent_request(node, parent_doc_id).await?;
            deny_if(
                parent.request_id == source && parent.agent_did == row.agent_did,
                "runtime-internal parent document does not match local source request",
            )?;
            let tool_call_id = row
                .caused_by_parent_tool_call_id
                .as_deref()
                .ok_or_else(|| {
                    AgentRequestAdmissionError::denied(anyhow::anyhow!(
                        "local-child runtime source has no tool-call binding"
                    ))
                })?;
            let tool_doc_id = row
                .caused_by_parent_tool_call_doc_id
                .as_deref()
                .ok_or_else(|| {
                    AgentRequestAdmissionError::denied(anyhow::anyhow!(
                        "runtime-internal parent tool-call document binding is absent"
                    ))
                })?;
            let target_name = verify_exact_parent_tool_call(
                node,
                tool_doc_id,
                tool_call_id,
                parent_doc_id,
                source,
                required_row_string(parent.agent_did.as_deref(), "agent_did")?,
                required_row_string(row.agent_did.as_deref(), "agent_did")?,
                target_behavior_id,
            )
            .await?;
            verify_exact_parent_subagent_policy(
                node,
                parent.behavior_id.as_deref(),
                &target_name,
                target_behavior_id,
                required_row_string(row.agent_did.as_deref(), "agent_did")?,
            )
            .await
        }
        RuntimeInternalSourceKind::CrossDeploymentChild => {
            let bridge_author_did = bridge_author_did.ok_or_else(|| {
                AgentRequestAdmissionError::denied(anyhow::anyhow!(
                    "cross-deployment child has no bridge author"
                ))
            })?;
            verify_cross_deployment_child_source(
                node,
                peer_admission,
                row,
                source,
                bridge_author_did,
                target_behavior_id,
            )
            .await
        }
        RuntimeInternalSourceKind::LocalControl => {
            deny_if(
                bridge_author_did.is_none()
                    && row.caused_by_parent_request_id.as_deref() == Some(source)
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
            let parent = load_exact_parent_request(node, parent_doc_id).await?;
            deny_if(
                parent.request_id == source && parent.agent_did == row.agent_did,
                "local-control parent document does not exactly own the source",
            )
        }
        RuntimeInternalSourceKind::AutomatedTrigger => {
            deny_if(
                bridge_author_did.is_none()
                    && row.caused_by_trigger_id.as_deref() == Some(source)
                    && matches!(
                        row.caused_by_trigger_kind.as_deref(),
                        Some("event" | "schedule")
                    ),
                "automated-trigger runtime source branch is mixed or incoherent",
            )?;
            verify_automated_trigger_source(
                node,
                row.caused_by_trigger_kind.as_deref().unwrap_or_default(),
                source,
                row.caused_by_trigger_doc_id.as_deref(),
                target_behavior_id,
            )
            .await
        }
    }
}

async fn load_exact_parent_request(
    node: &EmbeddedNode,
    source_doc_id: &str,
) -> AdmissionResult<AgentRequestRow> {
    let response = node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ request_id agent_did behavior_id }} }}"#,
        escape_graphql_string(source_doc_id),
    )).await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload runtime control parent failed: {:?}",
            response.errors
        )));
    }
    crate::graphql::first_row(&response, "AgentRequest")
        .map_err(AgentRequestAdmissionError::denied)?
        .ok_or_else(|| {
            AgentRequestAdmissionError::denied(anyhow::anyhow!(
                "runtime source parent document is missing"
            ))
        })
}

async fn verify_cross_deployment_child_source(
    node: &EmbeddedNode,
    peer_admission: &dyn PeerAdmissionAuthority,
    row: &AgentRequestRow,
    source: &str,
    bridge_author_did: &str,
    target_behavior_id: &str,
) -> AdmissionResult<()> {
    deny_if(
        row.caused_by_parent_request_id.as_deref() == Some(source),
        "cross-deployment child logical parent does not match signed source",
    )?;
    let parent_doc_id = row
        .caused_by_parent_request_doc_id
        .as_deref()
        .ok_or_else(|| {
            AgentRequestAdmissionError::denied(anyhow::anyhow!(
                "cross-deployment child has no opaque parent document binding"
            ))
        })?;
    let tool_call_id = row
        .caused_by_parent_tool_call_id
        .as_deref()
        .ok_or_else(|| {
            AgentRequestAdmissionError::denied(anyhow::anyhow!(
                "cross-deployment child has no parent tool-call binding"
            ))
        })?;
    let tool_doc_id = row
        .caused_by_parent_tool_call_doc_id
        .as_deref()
        .ok_or_else(|| {
            AgentRequestAdmissionError::denied(anyhow::anyhow!(
                "cross-deployment child has no physical tool-call binding"
            ))
        })?;

    #[derive(Deserialize)]
    struct BridgeRow {
        tool_call_id: Option<String>,
        request_id: Option<String>,
        request_doc_id: Option<String>,
        agent_did: Option<String>,
        spawn_target_did: Option<String>,
        child_request_id: Option<String>,
        args: Option<String>,
    }
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                tool_call_id request_id request_doc_id agent_did spawn_target_did child_request_id args
            }} }}"#,
            escape_graphql_string(tool_doc_id),
        ))
        .await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload cross-deployment source bridge failed: {:?}",
            response.errors
        )));
    }
    let bridge: BridgeRow = crate::graphql::first_row(&response, "AgentToolCall")
        .map_err(AgentRequestAdmissionError::denied)?
        .ok_or_else(|| {
            AgentRequestAdmissionError::denied(anyhow::anyhow!(
                "cross-deployment source bridge is missing"
            ))
        })?;
    deny_if(
        bridge.tool_call_id.as_deref() == Some(tool_call_id)
            && bridge.request_id.as_deref() == Some(source)
            && bridge.request_doc_id.as_deref() == Some(parent_doc_id)
            && bridge.agent_did.as_deref() == Some(bridge_author_did)
            && bridge.spawn_target_did.as_deref() == row.agent_did.as_deref()
            && bridge.child_request_id.as_deref() == Some(row.request_id.as_str()),
        "cross-deployment source bridge does not exactly own this child",
    )?;
    #[derive(Deserialize)]
    struct SpawnTargetArgs {
        behavior_id: String,
    }
    let args: SpawnTargetArgs = serde_json::from_str(bridge.args.as_deref().unwrap_or_default())
        .context("parse cross-deployment source bridge arguments")
        .map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        args.behavior_id == target_behavior_id
            && args.behavior_id == args.behavior_id.trim()
            && !args.behavior_id.is_empty(),
        "cross-deployment source bridge targets another behavior",
    )?;
    let authorized = peer_admission
        .fresh_member_authorized_for_agent(
            bridge_author_did,
            required_row_string(row.agent_did.as_deref(), "agent_did")?,
        )
        .await
        .context("reload cross-deployment bridge author admission")
        .map_err(AgentRequestAdmissionError::unavailable)?;
    deny_if(
        authorized,
        "cross-deployment bridge author is no longer authorized for the target",
    )?;
    verify_target_cross_deployment_policy(
        node,
        required_row_string(row.agent_did.as_deref(), "agent_did")?,
        target_behavior_id,
    )
    .await
}

async fn verify_target_cross_deployment_policy(
    node: &EmbeddedNode,
    target_agent_did: &str,
    target_behavior_id: &str,
) -> AdmissionResult<()> {
    #[derive(Deserialize)]
    struct BehaviorRow {
        tool_selection_id: Option<String>,
    }
    #[derive(Deserialize)]
    struct SelectionRow {
        subagent_allow_cross_deployment: Option<bool>,
    }
    let response = node
        .execute(&format!(
            r#"{{ AgentBehavior(filter: {{ behavior_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}, limit: 2) {{ tool_selection_id }} }}"#,
            escape_graphql_string(target_behavior_id),
            escape_graphql_string(target_agent_did),
        ))
        .await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload target cross-deployment behavior policy failed: {:?}",
            response.errors
        )));
    }
    let behaviors: Vec<BehaviorRow> = crate::graphql::rows(&response, "AgentBehavior")
        .map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        behaviors.len() == 1,
        "target cross-deployment behavior is missing or ambiguous",
    )?;
    let selection_id = behaviors[0].tool_selection_id.as_deref().ok_or_else(|| {
        AgentRequestAdmissionError::denied(anyhow::anyhow!(
            "target behavior has no cross-deployment policy"
        ))
    })?;
    deny_if(
        !selection_id.is_empty() && selection_id == selection_id.trim(),
        "target cross-deployment policy binding is noncanonical",
    )?;
    let response = node
        .execute(&format!(
            r#"{{ ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}, limit: 2) {{ subagent_allow_cross_deployment }} }}"#,
            escape_graphql_string(selection_id),
            escape_graphql_string(target_agent_did),
        ))
        .await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload target cross-deployment tool policy failed: {:?}",
            response.errors
        )));
    }
    let selections: Vec<SelectionRow> = crate::graphql::rows(&response, "ToolSelection")
        .map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        selections.len() == 1 && selections[0].subagent_allow_cross_deployment == Some(true),
        "target behavior no longer allows cross-deployment children",
    )
}

#[allow(clippy::too_many_arguments)]
async fn verify_exact_parent_tool_call(
    node: &EmbeddedNode,
    tool_doc_id: &str,
    tool_call_id: &str,
    parent_doc_id: &str,
    parent_request_id: &str,
    parent_agent_did: &str,
    target_agent_did: &str,
    target_behavior_id: &str,
) -> AdmissionResult<String> {
    #[derive(Deserialize)]
    struct ToolRow {
        tool_call_id: Option<String>,
        request_id: Option<String>,
        request_doc_id: Option<String>,
        agent_did: Option<String>,
        spawn_target_did: Option<String>,
        args: Option<String>,
    }
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
            tool_call_id request_id request_doc_id agent_did spawn_target_did args
        }} }}"#,
            escape_graphql_string(tool_doc_id),
        ))
        .await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload runtime source tool call failed: {:?}",
            response.errors
        )));
    }
    let tool: ToolRow = crate::graphql::first_row(&response, "AgentToolCall")
        .map_err(AgentRequestAdmissionError::denied)?
        .ok_or_else(|| {
            AgentRequestAdmissionError::denied(anyhow::anyhow!(
                "runtime source tool-call document is missing"
            ))
        })?;
    deny_if(
        tool.tool_call_id.as_deref() == Some(tool_call_id)
            && tool.request_id.as_deref() == Some(parent_request_id)
            && tool.request_doc_id.as_deref() == Some(parent_doc_id)
            && tool.agent_did.as_deref() == Some(parent_agent_did)
            && tool.spawn_target_did.as_deref() == Some(target_agent_did),
        "runtime source tool-call document does not exactly own this child",
    )?;
    #[derive(Deserialize)]
    struct SpawnTargetArgs {
        #[serde(default)]
        name: Option<String>,
        behavior_id: String,
    }
    let args: SpawnTargetArgs = serde_json::from_str(tool.args.as_deref().unwrap_or_default())
        .context("parse exact runtime source tool-call arguments")
        .map_err(AgentRequestAdmissionError::denied)?;
    let target_name = args.name.unwrap_or_else(|| args.behavior_id.clone());
    deny_if(
        args.behavior_id == target_behavior_id
            && target_name == target_name.trim()
            && !target_name.is_empty(),
        "runtime source tool-call target does not match child behavior",
    )?;
    Ok(target_name)
}

async fn verify_exact_parent_subagent_policy(
    node: &EmbeddedNode,
    parent_behavior_id: Option<&str>,
    target_name: &str,
    target_behavior_id: &str,
    target_agent_did: &str,
) -> AdmissionResult<()> {
    #[derive(Deserialize)]
    struct BehaviorPolicyRow {
        tool_selection_id: Option<String>,
    }
    #[derive(Deserialize)]
    struct SelectionPolicyRow {
        subagent_spawn_enabled: Option<bool>,
        subagent_targets: Option<Vec<String>>,
    }

    let parent_behavior_id = parent_behavior_id.ok_or_else(|| {
        AgentRequestAdmissionError::denied(anyhow::anyhow!(
            "exact runtime source parent has no behavior policy binding"
        ))
    })?;
    deny_if(
        parent_behavior_id == parent_behavior_id.trim() && !parent_behavior_id.is_empty(),
        "exact runtime source parent behavior binding is noncanonical",
    )?;
    let response = node
        .execute(&format!(
            r#"{{ AgentBehavior(filter: {{ behavior_id: {{ _eq: "{}" }} }}, limit: 2) {{ tool_selection_id }} }}"#,
            escape_graphql_string(parent_behavior_id),
        ))
        .await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload exact parent behavior policy failed: {:?}",
            response.errors
        )));
    }
    let behaviors: Vec<BehaviorPolicyRow> = crate::graphql::rows(&response, "AgentBehavior")
        .map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        behaviors.len() == 1,
        "exact parent behavior policy is missing or ambiguous",
    )?;
    let selection_id = behaviors[0].tool_selection_id.as_deref().ok_or_else(|| {
        AgentRequestAdmissionError::denied(anyhow::anyhow!(
            "exact parent behavior has no subagent policy"
        ))
    })?;
    deny_if(
        selection_id == selection_id.trim() && !selection_id.is_empty(),
        "exact parent subagent policy binding is noncanonical",
    )?;
    let response = node
        .execute(&format!(
            r#"{{ ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 2) {{ subagent_spawn_enabled subagent_targets }} }}"#,
            escape_graphql_string(selection_id),
        ))
        .await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload exact parent subagent policy failed: {:?}",
            response.errors
        )));
    }
    let selections: Vec<SelectionPolicyRow> = crate::graphql::rows(&response, "ToolSelection")
        .map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        selections.len() == 1 && selections[0].subagent_spawn_enabled == Some(true),
        "exact parent subagent policy is missing, ambiguous, or disabled",
    )?;
    let targets = selections[0]
        .subagent_targets
        .as_deref()
        .unwrap_or_default();
    let mut exact_matches = 0usize;
    for encoded in targets {
        let target: crate::document_config::SubagentTarget = serde_json::from_str(encoded)
            .context("parse exact parent subagent target policy")
            .map_err(AgentRequestAdmissionError::denied)?;
        deny_if(
            target.is_structurally_valid()
                && target.name == target.name.trim()
                && target.agent_did == target.agent_did.trim()
                && target.behavior_id == target.behavior_id.trim(),
            "exact parent subagent target policy is noncanonical",
        )?;
        if target.name == target_name
            && target.behavior_id == target_behavior_id
            && target.agent_did == target_agent_did
        {
            exact_matches += 1;
        }
    }
    deny_if(
        exact_matches == 1,
        "parent no longer exactly authorizes the runtime-internal target",
    )
}

async fn verify_automated_trigger_source(
    node: &EmbeddedNode,
    kind: &str,
    trigger_id: &str,
    trigger_doc_id: Option<&str>,
    target_behavior_id: &str,
) -> AdmissionResult<()> {
    #[derive(Deserialize)]
    struct TriggerRow {
        #[serde(alias = "trigger_id", alias = "schedule_id")]
        source_id: String,
        task_id: String,
        enabled: bool,
    }
    #[derive(Deserialize)]
    struct TaskRow {
        behavior_id: String,
        enabled: bool,
    }
    let collection = match kind {
        "event" => "EventTrigger",
        "schedule" => "Schedule",
        _ => {
            return Err(AgentRequestAdmissionError::denied(anyhow::anyhow!(
                "unsupported trigger kind"
            )))
        }
    };
    let id_field = match kind {
        "event" => "trigger_id",
        "schedule" => "schedule_id",
        _ => unreachable!(),
    };
    let trigger_doc_id = trigger_doc_id.ok_or_else(|| {
        AgentRequestAdmissionError::denied(anyhow::anyhow!(
            "runtime trigger configuration document binding is absent"
        ))
    })?;
    let response = node.execute(&format!(
        r#"{{ {collection}(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {id_field} task_id enabled }} }}"#,
        escape_graphql_string(trigger_doc_id),
    )).await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload runtime trigger source failed: {:?}",
            response.errors
        )));
    }
    let triggers: Vec<TriggerRow> =
        crate::graphql::rows(&response, collection).map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        triggers.len() == 1 && triggers[0].enabled && triggers[0].source_id == trigger_id,
        "runtime trigger source is missing, ambiguous, or disabled",
    )?;
    let task_response = node.execute(&format!(
        r#"{{ Task(filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 2) {{ behavior_id enabled }} }}"#,
        escape_graphql_string(&triggers[0].task_id),
    )).await;
    if task_response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload runtime trigger task failed: {:?}",
            task_response.errors
        )));
    }
    let tasks: Vec<TaskRow> =
        crate::graphql::rows(&task_response, "Task").map_err(AgentRequestAdmissionError::denied)?;
    deny_if(
        tasks.len() == 1 && tasks[0].enabled && tasks[0].behavior_id == target_behavior_id,
        "runtime trigger task is missing, disabled, ambiguous, or targets another behavior",
    )?;
    Ok(())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
fn require_pending_deadline_absent(deadline: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        deadline.is_none(),
        "pending AgentRequest carries a caller-authored execution deadline"
    );
    Ok(())
}

/// Verify the original immutable request payload and its declared admission
/// branch. Historical receipt authentication does not re-admit execution or
/// require today's enrollment, TTL, lifecycle, or backend readiness. The
/// operation's owner separately checks its expected principal/source scope.
pub(crate) fn verify_request_receipt_signature(row: &AgentRequestRow) -> Result<()> {
    let admission = row_admission(row)?;
    admission.validate_canonical_fields()?;
    admission
        .validate_branch_fields()
        .map_err(anyhow::Error::msg)?;
    let fields = row_signing_fields(row)?;
    validate_signing_fields(&fields)?;
    anyhow::ensure!(
        crate::identity::verify_did_signature(
            &admission.signer_did,
            &admission.signing_payload(&fields),
            &admission.signature,
        )?,
        "immutable request receipt signature is invalid"
    );
    Ok(())
}

/// Authenticate an already-authored runtime local-control receipt. This is
/// not fresh execution admission: terminal/expired children remain receipts.
/// The caller separately binds the exact goal, parent docID and sequence and
/// authorizes its own operation; no execution capability is returned here.
pub(crate) fn verify_runtime_local_control_receipt(
    row: &AgentRequestRow,
    expected_target_did: &str,
    expected_source_request_id: &str,
) -> Result<()> {
    let admission = row_admission(row)?;
    verify_request_receipt_signature(row)?;
    anyhow::ensure!(
        admission.kind == AgentRequestAdmissionKind::RuntimeInternal
            && admission.runtime_source_kind == Some(RuntimeInternalSourceKind::LocalControl)
            && admission.signer_did == expected_target_did
            && admission.runtime_issuer_did.as_deref() == Some(expected_target_did)
            && row.agent_did.as_deref() == Some(expected_target_did)
            && row.requester_did.as_deref() == Some(expected_target_did)
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
        row.runtime_bridge_author_did.as_deref(),
    )
    .map_err(anyhow::Error::msg)
}

fn row_signing_fields(row: &AgentRequestRow) -> Result<AgentRequestSigningFields<'_>> {
    let subagent_depth = row
        .subagent_depth
        .map(u32::try_from)
        .transpose()
        .context("AgentRequest subagent_depth must fit in u32")?
        .unwrap_or(0);
    Ok(AgentRequestSigningFields {
        request_id: &row.request_id,
        agent_did: row
            .agent_did
            .as_deref()
            .context("AgentRequest is missing agent_did")?,
        requester_did: row.requester_did.as_deref(),
        behavior_id: row.behavior_id.as_deref(),
        session_id: row
            .session_id
            .as_deref()
            .context("AgentRequest is missing session_id")?,
        retry_parent_request: row.retry_parent_request.as_deref(),
        retry_parent_request_doc_id: row.retry_parent_request_doc_id.as_deref(),
        retry_root_request: row.retry_root_request.as_deref(),
        retry_key: row.retry_key.as_deref(),
        content: row
            .content
            .as_deref()
            .context("AgentRequest is missing content")?,
        temperature: row.temperature,
        top_p: row.top_p,
        top_k: row.top_k,
        seed: row.seed,
        max_tokens: row.max_tokens,
        max_total_tokens: row.max_total_tokens,
        metadata: row.metadata.as_deref(),
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
        subagent_depth,
        caused_by_parent_request_id: row.caused_by_parent_request_id.as_deref(),
        caused_by_parent_request_doc_id: row.caused_by_parent_request_doc_id.as_deref(),
        caused_by_parent_tool_call_id: row.caused_by_parent_tool_call_id.as_deref(),
        caused_by_parent_tool_call_doc_id: row.caused_by_parent_tool_call_doc_id.as_deref(),
        workspace_id: row.workspace_id.as_deref(),
        workspace_authority: row.workspace_authority.as_deref(),
        workspace_owner_deployment_id: row.workspace_owner_deployment_id.as_deref(),
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

/// Complete immutable request signature projection shared by fresh admission
/// and transaction-scoped historical receipt readers. Lifecycle is included
/// for callers decoding the row, but is not part of the signed payload.
pub(crate) const SIGNED_REQUEST_FIELDS: &str = r#"
_docID lifecycle_state request_id agent_did requester_did behavior_id session_id
                retry_parent_request retry_parent_request_doc_id retry_root_request retry_key
                content temperature top_p top_k seed max_tokens max_total_tokens metadata
                execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_correlation
                caused_by_trigger_context caused_by_source_doc_id caused_by_trigger_doc_id
                created_at deadline retry_count
                max_retries valid_until subagent_depth caused_by_parent_request_id
                caused_by_parent_request_doc_id caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id workspace_id workspace_authority
                workspace_owner_deployment_id workspace_seal_hash admission_kind
                admission_signer_did admission_signature enrollment_request_id
                enrollment_request_digest enrollment_admin_did enrollment_authorization_sequence
                enrollment_authorization_expires_at runtime_issuer_did runtime_source_request_id
                runtime_source_kind runtime_bridge_author_did
"#;

async fn load_signed_request(
    node: &EmbeddedNode,
    doc_id: &str,
) -> AdmissionResult<AgentRequestRow> {
    let doc_id = escape_graphql_string(doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                {SIGNED_REQUEST_FIELDS}
            }} }}"#,
        ))
        .await;
    if response.has_errors() {
        return Err(AgentRequestAdmissionError::unavailable(anyhow::anyhow!(
            "reload AgentRequest admission row failed: {:?}",
            response.errors
        )));
    }
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
mod tests {
    use std::sync::Arc;

    use super::{require_pending_deadline_absent, AgentRequestAdmissionVerifier};
    use crate::agent::p2p_reconcile::enrollment_authority_channel;
    use crate::identity::{AgentIdentity, KeyIdentity};
    use crate::schema::ensure_runtime_schemas;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

    #[test]
    fn caller_authored_preclaim_deadline_fails_closed() {
        assert!(require_pending_deadline_absent(None).is_ok());
        assert!(require_pending_deadline_absent(Some(" ")).is_err());
        let error = require_pending_deadline_absent(Some("2099-01-01T00:00:00Z")).unwrap_err();
        assert!(error
            .to_string()
            .contains("caller-authored execution deadline"));
    }

    #[tokio::test]
    async fn runtime_receipt_authenticates_terminal_expired_child_and_rejects_forgery() {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("receipt.key"), None).unwrap();
        let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
        ensure_runtime_schemas(&node).await.unwrap();
        let mut create = AgentRequestCreate::base(
            "receipt-child",
            identity.did(),
            identity.did(),
            "behavior-1",
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
        super::verify_runtime_local_control_receipt(&row, identity.did(), "receipt-parent")
            .expect("terminal and expired original receipt remains authenticated");
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "different-parent"
        )
        .is_err());
        let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap();
        assert!(
            super::verify_runtime_local_control_receipt(&row, foreign.did(), "receipt-parent")
                .is_err()
        );
        row.content = Some("forged continuation".into());
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent"
        )
        .is_err());
        row.content = Some("original continuation".into());
        row.admission_signature = Some("not-a-valid-signature".into());
        assert!(super::verify_runtime_local_control_receipt(
            &row,
            identity.did(),
            "receipt-parent"
        )
        .is_err());
        node.shutdown().await;
    }

    #[tokio::test]
    async fn final_verifier_returns_the_exact_fresh_signed_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn AgentIdentity> =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap());
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        ensure_runtime_schemas(node.as_ref()).await.unwrap();

        let request_id = uuid::Uuid::new_v4().to_string();
        let mut create = AgentRequestCreate::base(
            request_id,
            identity.did(),
            identity.did(),
            "behavior-1",
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

        let (_authority_owner, authority) = enrollment_authority_channel();
        let verifier = AgentRequestAdmissionVerifier::new(node.clone(), identity, authority);
        let verified = verifier.verify_fresh(&queued, "behavior-1").await.unwrap();
        assert_eq!(verified.content, "durable signed content");
        assert_ne!(verified.content, queued.content);

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
        let error = verifier
            .verify_fresh(&queued, "behavior-1")
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("caller-authored execution deadline"));
    }

    #[tokio::test]
    async fn runtime_trigger_source_requires_current_trigger_and_target_policy() {
        let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
        ensure_runtime_schemas(&node).await.unwrap();
        let response = node
            .execute(
                r#"mutation {
                    task: create_Task(input: {
                        task_id: "task-1", name: "task-1", behavior_id: "behavior-1",
                        prompt_template: "run", enabled: true
                    }) { _docID }
                    trigger: create_EventTrigger(input: {
                        trigger_id: "trigger-1", task_id: "task-1",
                        source_collection: "AgentRequest", event_kind: "created",
                        filter: "{}", enabled: true, concurrency: "serial", fire_count: 0
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
            "behavior-1",
        )
        .await
        .unwrap();

        let response = node
            .execute(
                r#"mutation {
                    update_EventTrigger(
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
            "behavior-1"
        )
        .await
        .is_err());

        let response = node
            .execute(
                r#"mutation {
                    update_EventTrigger(
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
            "behavior-1"
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn exact_subagent_policy_distinguishes_transport_retry_from_policy_denial() {
        let unavailable_node = defra_node::EmbeddedNode::builder().build().await.unwrap();
        let unavailable = super::verify_exact_parent_subagent_policy(
            &unavailable_node,
            Some("parent-behavior"),
            "researcher",
            "research",
            "did:key:target",
        )
        .await
        .unwrap_err();
        assert!(!unavailable.is_denied(), "schema/query failure must retry");

        let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
        ensure_runtime_schemas(&node).await.unwrap();
        let denied = super::verify_exact_parent_subagent_policy(
            &node,
            Some("parent-behavior"),
            "researcher",
            "research",
            "did:key:target",
        )
        .await
        .unwrap_err();
        assert!(denied.is_denied(), "missing policy must terminally deny");

        let target = crate::document_config::subagent_target_entry(
            "researcher",
            "did:key:target",
            "research",
            None,
        );
        let response = node
            .execute(&format!(
                r#"mutation {{
                    selection: create_ToolSelection(input: {{
                        selection_id: "parent-selection",
                        subagent_spawn_enabled: true,
                        subagent_targets: ["{}"]
                    }}) {{ _docID }}
                    behavior: create_AgentBehavior(input: {{
                        behavior_id: "parent-behavior", agent_did: "did:key:parent",
                        tool_selection_id: "parent-selection", enabled: true
                    }}) {{ _docID }}
                }}"#,
                crate::graphql::escape_graphql_string(&target),
            ))
            .await;
        assert!(
            !response.has_errors(),
            "seed exact policy: {:?}",
            response.errors
        );
        super::verify_exact_parent_subagent_policy(
            &node,
            Some("parent-behavior"),
            "researcher",
            "research",
            "did:key:target",
        )
        .await
        .unwrap();
    }
}
