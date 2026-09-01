//! Durable enrollment document loader and fail-closed authority projector.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use defra_p2p_adapter::{P2pDocumentRequest, TransportPeerId};
use gents_protocol::enrollment::{
    decode_offer, derive_decision_id, derive_revision_id, derive_route_receipt_id,
    enrollment_schema_fingerprint, AuthorizationRevisionKind as WireRevisionKind,
    AuthorizationRevisionRecord, EnrollmentDecisionKind as WireDecisionKind,
    EnrollmentDecisionRecord, EnrollmentRequestRecord,
    EnrollmentRouteReceiptDirection as WireReceiptDirection, EnrollmentRouteReceiptRecord,
    DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS, ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::NetworkRecord;
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::identity::AgentIdentity;

use super::enrollment::{
    AuthorizationRevision, AuthorizationRevisionKind, DurableEnrollmentDocuments,
    EnrollmentDecision, EnrollmentDecisionKind, EnrollmentOffer, EnrollmentRequest,
    EnrollmentRouteDirection, EnrollmentRouteReceipt, NetworkAdminPin,
};
use super::graphql_helpers::{ensure_no_errors, rows};

const ENROLLMENT_DOCUMENT_QUERY: &str = r#"{
  AgentNetwork { network_id admin_did display_name default_template created_at admin_sig }
  NetworkEnrollmentRequest {
    _docID protocol_version request_id request_digest offer_id offer_token challenge
    network_id admin_did server_peer candidate_did candidate_peer candidate_ticket
    owner_agent profile client_nonce issued_at expires_at candidate_sig
  }
  NetworkEnrollmentDecision {
    _docID protocol_version decision_id request_id request_digest network_id admin_did
    candidate_did candidate_peer owner_agent decision authorization_sequence
    authorization_expires_at decided_at
    signer_did admin_sig
  }
  NetworkAuthorizationRevision {
    _docID protocol_version revision_id request_id request_digest network_id admin_did
    member_did member_peer owner_agent sequence authorization_expires_at kind issued_at signer_did admin_sig
  }
  NetworkEnrollmentRouteReceipt {
    _docID protocol_version receipt_id request_id request_digest network_id admin_did
    member_did member_peer server_peer owner_agent authorization_sequence authorization_expires_at direction
    applied_at signer_did admin_sig
  }
}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEnrollment {
    pub request_doc_id: String,
    pub offer_token: String,
    pub request: EnrollmentRequestRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEnrollment {
    pub request_doc_id: String,
    pub decision_doc_id: String,
    pub revision_doc_id: String,
    pub route_receipt_doc_id: Option<String>,
    pub request: EnrollmentRequestRecord,
    pub decision: EnrollmentDecisionRecord,
    pub revision: AuthorizationRevisionRecord,
    pub route_receipt: Option<EnrollmentRouteReceiptRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedEnrollment {
    pub request_doc_id: String,
    pub decision_doc_id: String,
    pub request: EnrollmentRequestRecord,
    pub decision: EnrollmentDecisionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedEnrollment {
    pub request_doc_id: String,
    pub decision_doc_id: String,
    pub revision_doc_id: String,
    pub request: EnrollmentRequestRecord,
    pub decision: EnrollmentDecisionRecord,
    pub revision: AuthorizationRevisionRecord,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrollmentProjection {
    pub network_id: Option<String>,
    pub pending: Vec<PendingEnrollment>,
    pub active: Vec<ActiveEnrollment>,
    pub denied: Vec<DeniedEnrollment>,
    /// Exact unique maximal revoked tombstones retained for direct redelivery.
    pub revoked: Vec<RevokedEnrollment>,
    pub next_authorization_sequences: BTreeMap<(String, String), u64>,
    /// Corruption whose ownership is attributable to one network member.
    /// It retracts only that member's current route.
    pub scoped_conflicts: BTreeMap<(String, String), String>,
    /// Immutable request identity/terminal conflicts. A conflicted request is
    /// never decidable, but another valid request for the member remains so.
    pub request_conflicts: BTreeMap<String, String>,
    /// Root/unattributable local-authority corruption only.
    pub conflict: Option<String>,
}

impl EnrollmentProjection {
    fn conflicted(network_id: Option<String>, error: impl std::fmt::Display) -> Self {
        Self {
            network_id,
            conflict: Some(error.to_string()),
            ..Self::default()
        }
    }

    fn pending_for_decision(&self, request_id: &str) -> Result<&PendingEnrollment> {
        if let Some(reason) = self.request_conflicts.get(request_id) {
            anyhow::bail!("enrollment request {request_id} is conflicted: {reason}");
        }
        self.pending
            .iter()
            .find(|pending| pending.request.request_id == request_id)
            .with_context(|| format!("no fresh pending enrollment request {request_id}"))
    }
}

#[derive(Clone)]
pub struct GraphqlEnrollmentStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    decision_lock: Arc<Mutex<()>>,
}

fn enrollment_decision_gate(node: &EmbeddedNode) -> Arc<Mutex<()>> {
    static GATES: OnceLock<StdMutex<BTreeMap<usize, Weak<Mutex<()>>>>> = OnceLock::new();
    let key = node as *const EmbeddedNode as usize;
    let mut gates = GATES
        .get_or_init(|| StdMutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}

impl GraphqlEnrollmentStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        let decision_lock = enrollment_decision_gate(node.as_ref());
        Self {
            node,
            identity,
            decision_lock,
        }
    }

    pub async fn decide_request(
        &self,
        request_id: &str,
        kind: WireDecisionKind,
    ) -> Result<EnrollmentDecisionOutcome> {
        self.decide_request_with_lease(
            request_id,
            kind,
            Duration::from_secs(DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS),
        )
        .await
    }

    pub async fn decide_request_with_lease(
        &self,
        request_id: &str,
        kind: WireDecisionKind,
        lease: Duration,
    ) -> Result<EnrollmentDecisionOutcome> {
        let _guard = self.decision_lock.lock().await;
        let projection = self.load_projection().await?;
        anyhow::ensure!(
            projection.conflict.is_none(),
            "enrollment authority is conflicted: {}",
            projection.conflict.as_deref().unwrap_or("unknown conflict")
        );
        if let Some(active) = projection
            .active
            .iter()
            .find(|active| active.request.request_id == request_id)
        {
            anyhow::ensure!(
                kind == WireDecisionKind::Approved,
                "request {request_id} is already approved"
            );
            let request = active.request.clone();
            let decision_doc_id = active.decision_doc_id.clone();
            let revision_doc_id = active.revision_doc_id.clone();
            let route_receipt_doc_id = active.route_receipt_doc_id.clone();
            drop(_guard);
            let delivery_pending = self
                .deliver_terminal(
                    &request,
                    &decision_doc_id,
                    Some(&revision_doc_id),
                    route_receipt_doc_id.as_deref(),
                )
                .await;
            return Ok(EnrollmentDecisionOutcome {
                request_id: request_id.to_string(),
                state: "approved",
                decision_doc_id,
                revision_doc_id: Some(revision_doc_id),
                delivery_pending,
            });
        }
        if let Some(denied) = projection
            .denied
            .iter()
            .find(|denied| denied.request.request_id == request_id)
        {
            anyhow::ensure!(
                kind == WireDecisionKind::Denied,
                "request {request_id} is already denied"
            );
            let request = denied.request.clone();
            let decision_doc_id = denied.decision_doc_id.clone();
            drop(_guard);
            let delivery_pending = self
                .deliver_terminal(&request, &decision_doc_id, None, None)
                .await;
            return Ok(EnrollmentDecisionOutcome {
                request_id: request_id.to_string(),
                state: "denied",
                decision_doc_id,
                revision_doc_id: None,
                delivery_pending,
            });
        }
        let pending = projection.pending_for_decision(request_id)?;

        let decided_at = self.verify_live_candidate(&pending.request).await?;
        let sequence = match kind {
            WireDecisionKind::Approved => projection
                .next_authorization_sequences
                .get(&(
                    pending.request.network_id.clone(),
                    pending.request.candidate_did.clone(),
                ))
                .copied()
                .unwrap_or(1),
            WireDecisionKind::Denied => 0,
        };
        anyhow::ensure!(
            sequence <= i64::MAX as u64,
            "authorization sequence exhausted the DefraDB Int range for this member"
        );
        let decided_at = decided_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let authorization_expires_at = match kind {
            WireDecisionKind::Approved => (DateTime::parse_from_rfc3339(&decided_at)?
                + chrono::Duration::from_std(lease)?)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            // DateTime fields cannot encode an empty sentinel. A denial has
            // no lease; its signed boundary is exactly its decision time.
            WireDecisionKind::Denied => decided_at.clone(),
        };
        let mut decision = EnrollmentDecisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            decision_id: derive_decision_id(
                &pending.request.request_id,
                &pending.request.request_digest,
            ),
            request_id: pending.request.request_id.clone(),
            request_digest: pending.request.request_digest.clone(),
            network_id: pending.request.network_id.clone(),
            admin_did: pending.request.admin_did.clone(),
            candidate_did: pending.request.candidate_did.clone(),
            candidate_peer: pending.request.candidate_peer.clone(),
            owner_agent: pending.request.owner_agent.clone(),
            decision: kind.clone(),
            authorization_sequence: sequence,
            authorization_expires_at: authorization_expires_at.clone(),
            decided_at: decided_at.clone(),
            signer_did: self.identity.did().to_string(),
            admin_sig: Vec::new(),
        };
        decision.admin_sig = self.identity.sign(&decision.signing_payload()).await?;
        decision.validate_against_request(&pending.request)?;

        let mut revision = (kind == WireDecisionKind::Approved).then(|| {
            let revision_kind = WireRevisionKind::Active;
            AuthorizationRevisionRecord {
                protocol_version: ENROLLMENT_PROTOCOL_VERSION,
                revision_id: derive_revision_id(
                    &pending.request.network_id,
                    &pending.request.candidate_did,
                    sequence,
                    &revision_kind,
                    &pending.request.request_digest,
                ),
                request_id: pending.request.request_id.clone(),
                request_digest: pending.request.request_digest.clone(),
                network_id: pending.request.network_id.clone(),
                admin_did: pending.request.admin_did.clone(),
                member_did: pending.request.candidate_did.clone(),
                member_peer: pending.request.candidate_peer.clone(),
                owner_agent: pending.request.owner_agent.clone(),
                sequence,
                authorization_expires_at: authorization_expires_at.clone(),
                kind: revision_kind,
                issued_at: decided_at,
                signer_did: self.identity.did().to_string(),
                admin_sig: Vec::new(),
            }
        });
        if let Some(record) = revision.as_mut() {
            record.admin_sig = self.identity.sign(&record.signing_payload()).await?;
            record.validate_against_approval(&pending.request, &decision)?;
        }

        let mutation = decision_mutation(&decision, revision.as_ref());
        let committed = async {
            let response = crate::graphql::graphql_mutation_with_transaction_retry(
                self.node.as_ref(),
                &mutation,
                "write enrollment operator decision",
            )
            .await?;
            let response = json!({ "data": response.data.unwrap_or_default() });
            let decision_doc_id = gents_protocol::graphql::extract_mutation_doc_id(
                &response,
                "NetworkEnrollmentDecision",
            )?;
            let revision_doc_id = revision
                .as_ref()
                .map(|_| {
                    gents_protocol::graphql::extract_mutation_doc_id(
                        &response,
                        "NetworkAuthorizationRevision",
                    )
                })
                .transpose()?;
            Ok::<_, anyhow::Error>((decision_doc_id, revision_doc_id))
        }
        .await;
        let (decision_doc_id, revision_doc_id) = match committed {
            Ok(committed) => committed,
            Err(commit_error) => self
                .recover_exact_terminal(&decision, revision.as_ref())
                .await
                .with_context(|| {
                    format!(
                        "enrollment terminal commit was not observably recovered after: {commit_error:#}"
                    )
                })?,
        };
        drop(_guard);
        let delivery_pending = match &revision_doc_id {
            Some(revision_doc_id) => {
                self.deliver_terminal(
                    &pending.request,
                    &decision_doc_id,
                    Some(revision_doc_id),
                    None,
                )
                .await
            }
            None => {
                self.deliver_terminal(&pending.request, &decision_doc_id, None, None)
                    .await
            }
        };
        Ok(EnrollmentDecisionOutcome {
            request_id: request_id.to_string(),
            state: match kind {
                WireDecisionKind::Approved => "approved",
                WireDecisionKind::Denied => "denied",
            },
            decision_doc_id,
            revision_doc_id,
            delivery_pending,
        })
    }

    /// Append an admin-signed maximal revocation and directly redeliver the
    /// tombstone. Exact replay is idempotent; authority history is never deleted.
    pub async fn revoke_request(&self, request_id: &str) -> Result<EnrollmentDecisionOutcome> {
        let guard = self.decision_lock.lock().await;
        let projection = self.load_projection().await?;
        anyhow::ensure!(
            projection.conflict.is_none(),
            "enrollment authority is conflicted: {}",
            projection.conflict.as_deref().unwrap_or("unknown conflict")
        );
        if let Some(revoked) = projection
            .revoked
            .iter()
            .find(|revoked| revoked.request.request_id == request_id)
        {
            let revoked = revoked.clone();
            drop(guard);
            let delivery_pending = self
                .deliver_terminal(
                    &revoked.request,
                    &revoked.decision_doc_id,
                    Some(&revoked.revision_doc_id),
                    None,
                )
                .await;
            return Ok(EnrollmentDecisionOutcome {
                request_id: request_id.to_string(),
                state: "revoked",
                decision_doc_id: revoked.decision_doc_id,
                revision_doc_id: Some(revoked.revision_doc_id),
                delivery_pending,
            });
        }
        let active = projection
            .active
            .iter()
            .find(|active| active.request.request_id == request_id)
            .cloned()
            .with_context(|| format!("request {request_id} has no current approval to revoke"))?;
        let scope = (
            active.request.network_id.clone(),
            active.request.candidate_did.clone(),
        );
        let sequence = projection
            .next_authorization_sequences
            .get(&scope)
            .copied()
            .unwrap_or_else(|| active.revision.sequence.saturating_add(1));
        anyhow::ensure!(
            sequence > active.revision.sequence && sequence <= i64::MAX as u64,
            "authorization revocation sequence is invalid or exhausted"
        );
        let kind = WireRevisionKind::Revoked;
        let issued_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut revision = AuthorizationRevisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            revision_id: derive_revision_id(
                &active.request.network_id,
                &active.request.candidate_did,
                sequence,
                &kind,
                &active.request.request_digest,
            ),
            request_id: active.request.request_id.clone(),
            request_digest: active.request.request_digest.clone(),
            network_id: active.request.network_id.clone(),
            admin_did: active.request.admin_did.clone(),
            member_did: active.request.candidate_did.clone(),
            member_peer: active.request.candidate_peer.clone(),
            owner_agent: active.request.owner_agent.clone(),
            sequence,
            authorization_expires_at: active.decision.authorization_expires_at.clone(),
            kind,
            issued_at,
            signer_did: self.identity.did().to_string(),
            admin_sig: Vec::new(),
        };
        revision.admin_sig = self.identity.sign(&revision.signing_payload()).await?;
        revision.validate_against_approval(&active.request, &active.decision)?;
        let mutation = revision_mutation(&revision);
        let response = crate::graphql::graphql_mutation_with_transaction_retry(
            self.node.as_ref(),
            &mutation,
            "write enrollment revocation",
        )
        .await;
        let revision_doc_id = match response {
            Ok(response) => {
                let response = json!({ "data": response.data.unwrap_or_default() });
                gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "NetworkAuthorizationRevision",
                )?
            }
            Err(commit_error) => {
                let recovered = self.load_projection().await?;
                recovered
                    .revoked
                    .iter()
                    .find(|candidate| candidate.revision == revision)
                    .map(|candidate| candidate.revision_doc_id.clone())
                    .with_context(|| {
                        format!(
                        "revocation commit was not observably recovered after: {commit_error:#}"
                    )
                    })?
            }
        };
        drop(guard);
        let delivery_pending = self
            .deliver_terminal(
                &active.request,
                &active.decision_doc_id,
                Some(&revision_doc_id),
                None,
            )
            .await;
        Ok(EnrollmentDecisionOutcome {
            request_id: request_id.to_string(),
            state: "revoked",
            decision_doc_id: active.decision_doc_id,
            revision_doc_id: Some(revision_doc_id),
            delivery_pending,
        })
    }

    async fn verify_live_candidate(
        &self,
        request: &EnrollmentRequestRecord,
    ) -> Result<DateTime<Utc>> {
        let p2p = self.node.p2p_arc().context("runtime P2P is unavailable")?;
        let local_peer = timeout(Duration::from_secs(20), p2p.local_peer_id())
            .await
            .context("timed out reading the enrollment server peer identity")?
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            local_peer == request.server_peer,
            "enrollment request targets a different server transport peer"
        );
        timeout(
            Duration::from_secs(20),
            p2p.connect_peer(&request.candidate_ticket),
        )
        .await
        .context("timed out connecting to enrollment candidate")?
        .map_err(anyhow::Error::msg)?;
        let peer =
            TransportPeerId::new(request.candidate_peer.clone()).map_err(anyhow::Error::msg)?;
        let resolved = timeout(Duration::from_secs(20), p2p.resolve_peer_identity(&peer))
            .await
            .context("timed out resolving enrollment candidate identity")?
            .map_err(anyhow::Error::msg)?
            .context("enrollment candidate has no authenticated identity")?;
        anyhow::ensure!(
            resolved.to_string() == request.candidate_did,
            "candidate transport identity does not match its signed request DID"
        );
        let now = Utc::now();
        let issued_at = DateTime::parse_from_rfc3339(&request.issued_at)
            .context("parsing enrollment request issued_at")?
            .with_timezone(&Utc);
        let expires_at = DateTime::parse_from_rfc3339(&request.expires_at)
            .context("parsing enrollment request expires_at")?
            .with_timezone(&Utc);
        anyhow::ensure!(issued_at <= now, "enrollment request is not yet valid");
        anyhow::ensure!(
            now <= expires_at,
            "enrollment request expired during approval"
        );
        Ok(now)
    }

    async fn recover_exact_terminal(
        &self,
        decision: &EnrollmentDecisionRecord,
        revision: Option<&AuthorizationRevisionRecord>,
    ) -> Result<(String, Option<String>)> {
        let projection = self.load_projection().await?;
        match revision {
            Some(revision) => {
                let active = projection
                    .active
                    .iter()
                    .find(|active| active.decision == *decision && active.revision == *revision)
                    .context("exact committed approval pair is absent")?;
                Ok((
                    active.decision_doc_id.clone(),
                    Some(active.revision_doc_id.clone()),
                ))
            }
            None => {
                let denied = projection
                    .denied
                    .iter()
                    .find(|denied| denied.decision == *decision)
                    .context("exact committed denial is absent")?;
                Ok((denied.decision_doc_id.clone(), None))
            }
        }
    }

    /// Persist the immutable receipt for the exact authorization generation
    /// after the pairing owner has reported the runtime-side route applied.
    pub(crate) async fn record_applied_route(
        &self,
        active: &ActiveEnrollment,
        applied_at: DateTime<Utc>,
    ) -> Result<String> {
        if let Some(doc_id) = &active.route_receipt_doc_id {
            return Ok(doc_id.clone());
        }
        let current = self.load_projection().await?;
        anyhow::ensure!(
            current.active.iter().any(|candidate| {
                candidate.request == active.request
                    && candidate.decision == active.decision
                    && candidate.revision == active.revision
            }),
            "authorization generation expired or changed before route receipt publication"
        );
        let direction = WireReceiptDirection::ClientToServer;
        let mut receipt = EnrollmentRouteReceiptRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            receipt_id: derive_route_receipt_id(
                &active.request.request_id,
                &active.request.request_digest,
                active.decision.authorization_sequence,
                &direction,
            ),
            request_id: active.request.request_id.clone(),
            request_digest: active.request.request_digest.clone(),
            network_id: active.request.network_id.clone(),
            admin_did: active.request.admin_did.clone(),
            member_did: active.request.candidate_did.clone(),
            member_peer: active.request.candidate_peer.clone(),
            server_peer: active.request.server_peer.clone(),
            owner_agent: active.request.owner_agent.clone(),
            authorization_sequence: active.decision.authorization_sequence,
            authorization_expires_at: active.decision.authorization_expires_at.clone(),
            direction,
            applied_at: applied_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            signer_did: self.identity.did().to_string(),
            admin_sig: Vec::new(),
        };
        receipt.admin_sig = self.identity.sign(&receipt.signing_payload()).await?;
        receipt.validate_against_approval(&active.request, &active.decision)?;
        let mutation = route_receipt_mutation(&receipt);
        let committed = crate::graphql::graphql_mutation_with_transaction_retry(
            self.node.as_ref(),
            &mutation,
            "write enrollment route receipt",
        )
        .await;
        match committed {
            Ok(response) => {
                let response = json!({ "data": response.data.unwrap_or_default() });
                gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "NetworkEnrollmentRouteReceipt",
                )
            }
            Err(commit_error) => {
                let projection = self.load_projection().await?;
                projection
                    .active
                    .iter()
                    .find(|candidate| candidate.route_receipt.as_ref() == Some(&receipt))
                    .and_then(|candidate| candidate.route_receipt_doc_id.clone())
                    .with_context(|| {
                        format!(
                            "route receipt commit was not observably recovered after: {commit_error:#}"
                        )
                    })
            }
        }
    }

    pub(crate) async fn deliver_terminal(
        &self,
        request: &EnrollmentRequestRecord,
        decision_doc_id: &str,
        revision_doc_id: Option<&str>,
        route_receipt_doc_id: Option<&str>,
    ) -> bool {
        // Delivery is a capability handoff. Reload immediately before push so
        // an earlier active sweep can never race a durable revocation/new max.
        let current = match self.load_projection().await {
            Ok(projection) => projection,
            Err(error) => {
                tracing::warn!(error = %error, request_id = %request.request_id,
                    "enrollment terminal delivery fence reload failed");
                return true;
            }
        };
        let exact_current = terminal_is_exact_current(
            &current,
            request,
            decision_doc_id,
            revision_doc_id,
            route_receipt_doc_id,
        );
        if !exact_current {
            tracing::warn!(request_id = %request.request_id,
                "suppressed stale enrollment terminal delivery");
            return true;
        }
        let Some(p2p) = self.node.p2p_arc() else {
            return true;
        };
        let documents = terminal_documents(decision_doc_id, revision_doc_id, route_receipt_doc_id);
        let delivery = async {
            p2p.connect_peer(&request.candidate_ticket)
                .await
                .map_err(anyhow::Error::msg)?;
            p2p.push_documents_to_peer(&request.candidate_peer, documents)
                .await
                .map_err(anyhow::Error::msg)
        };
        match timeout(Duration::from_secs(20), delivery).await {
            Ok(Ok(())) => false,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, request_id = %request.request_id, "enrollment terminal committed; direct delivery will retry");
                true
            }
            Err(_) => {
                tracing::warn!(request_id = %request.request_id, "enrollment terminal committed; direct delivery timed out and will retry");
                true
            }
        }
    }

    /// Load every authority row once and project it without relying on row order.
    pub async fn load_projection(&self) -> Result<EnrollmentProjection> {
        let response = self.node.execute(ENROLLMENT_DOCUMENT_QUERY).await;
        let now = Utc::now();
        ensure_no_errors(&response, "query authenticated enrollment documents")?;
        let raw_network_rows = rows::<Value>(&response, "AgentNetwork")?;
        let network_id = raw_network_rows
            .first()
            .and_then(|row| row.get("network_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let network_rows = raw_network_rows
            .into_iter()
            .map(serde_json::from_value::<AgentNetworkRow>)
            .collect::<std::result::Result<Vec<_>, _>>();
        let network_rows = match network_rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(error = %error, "enrollment root authority projected fail closed");
                return Ok(EnrollmentProjection::conflicted(network_id, error));
            }
        };
        match self.project_response(&response, &network_rows, now).await {
            Ok(projection) => Ok(projection),
            Err(error) => {
                tracing::warn!(error = %error, "enrollment authority projected fail closed");
                Ok(EnrollmentProjection::conflicted(network_id, error))
            }
        }
    }

    async fn project_response(
        &self,
        response: &query::QueryResponse,
        network_rows: &[AgentNetworkRow],
        now: DateTime<Utc>,
    ) -> Result<EnrollmentProjection> {
        let [network_row] = network_rows else {
            anyhow::bail!(
                "expected exactly one AgentNetwork, found {}",
                network_rows.len()
            );
        };
        let network = network_row.to_record()?;
        anyhow::ensure!(
            network.admin_did == self.identity.did(),
            "AgentNetwork admin does not match the local runtime identity"
        );
        anyhow::ensure!(
            self.identity
                .verify(&network.admin_did, &network.signing_payload(), &network.sig)
                .await?,
            "AgentNetwork signature is invalid"
        );

        let mut scoped_conflicts = BTreeMap::<(String, String), Vec<String>>::new();
        let mut request_conflicts = BTreeMap::<String, Vec<String>>::new();
        let mut request_scopes = BTreeMap::<String, (String, String)>::new();
        let mut requests = Vec::new();
        for raw in rows::<Value>(response, "NetworkEnrollmentRequest")? {
            let (request_id, scope) = match attribute_candidate_request(&raw, &network.network_id) {
                CandidateRequestAttribution::Exact { request_id, scope } => (request_id, scope),
                CandidateRequestAttribution::RequestOnly(request_id) => {
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        "malformed enrollment request has no candidate_did",
                    );
                    continue;
                }
                CandidateRequestAttribution::ScopeOnly(scope) => {
                    add_scope_conflict(
                        &mut scoped_conflicts,
                        scope,
                        "malformed enrollment request has no request_id",
                    );
                    continue;
                }
                CandidateRequestAttribution::ForeignOrUnattributable => {
                    tracing::warn!(
                        "quarantining enrollment request without request or member identity"
                    );
                    continue;
                }
            };
            if let Some(existing) = request_scopes.insert(request_id.clone(), scope.clone()) {
                if existing != scope {
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        "request identity is bound to conflicting member scopes",
                    );
                    tracing::warn!(
                        ?existing,
                        ?scope,
                        request_id,
                        "immutable request identity is bound to conflicting member scopes"
                    );
                }
            }
            let row = match serde_json::from_value::<RequestRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        format!("malformed enrollment request: {error}"),
                    );
                    continue;
                }
            };
            match self.verify_request(row, &network).await {
                Ok(request) => requests.push(request),
                Err(error) => add_request_conflict(
                    &mut request_conflicts,
                    &request_id,
                    format!("invalid enrollment request: {error:#}"),
                ),
            }
        }
        let requests_by_id = requests.iter().fold(BTreeMap::new(), |mut map, verified| {
            map.entry(verified.record.request_id.clone())
                .or_insert_with(Vec::new)
                .push(verified);
            map
        });
        for (request_id, rows) in &requests_by_id {
            if rows.len() != 1 {
                add_request_conflict(
                    &mut request_conflicts,
                    request_id,
                    format!("request identity has {} distinct rows", rows.len()),
                );
            }
        }
        let mut challenge_requests = BTreeMap::<String, BTreeSet<String>>::new();
        for request in &requests {
            challenge_requests
                .entry(request.record.challenge.clone())
                .or_default()
                .insert(request.record.request_id.clone());
        }
        for request_ids in challenge_requests.values().filter(|ids| ids.len() > 1) {
            for request_id in request_ids {
                add_request_conflict(
                    &mut request_conflicts,
                    request_id,
                    "enrollment challenge is reused by another request",
                );
            }
        }

        let mut decisions = Vec::new();
        // Structurally attributable hostile rows retain their observed maximum
        // without poisoning unrelated members. A later valid generation may recover.
        let mut invalid_authority_max = BTreeMap::<(String, String), u64>::new();
        let mut observed_revision_next = BTreeMap::<(String, String), u64>::new();
        for raw in rows::<Value>(response, "NetworkEnrollmentDecision")? {
            let request_id = raw_text(&raw, "request_id").map(str::to_string);
            let request_key = request_id.as_deref().unwrap_or("");
            let Some(scope) = scope_for_authority_row(
                &raw,
                request_key,
                &request_scopes,
                &network.network_id,
                "candidate_did",
            )?
            else {
                continue;
            };
            let observed_sequence =
                raw_i64(&raw, "authorization_sequence").and_then(|value| u64::try_from(value).ok());
            if let Some(sequence) = observed_sequence {
                observe_next_authorization_sequence(
                    &mut observed_revision_next,
                    &mut scoped_conflicts,
                    scope.clone(),
                    sequence,
                );
            }
            let Some(request_id) = request_id else {
                if let Some(sequence) = observed_sequence {
                    invalid_authority_max
                        .entry(scope)
                        .and_modify(|current| *current = (*current).max(sequence))
                        .or_insert(sequence);
                } else {
                    add_scope_conflict(
                        &mut scoped_conflicts,
                        scope,
                        "malformed enrollment decision has no request_id or usable generation",
                    );
                }
                continue;
            };
            let row = match serde_json::from_value::<DecisionRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    }
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        format!("malformed enrollment decision: {error}"),
                    );
                    continue;
                }
            };
            let verified = async {
                let record = row.to_record()?;
                let request = unique_request(&requests_by_id, &record.request_id)?;
                record.validate_against_request(&request.record)?;
                let signed = self
                    .identity
                    .verify(
                        &record.signer_did,
                        &record.signing_payload(),
                        &record.admin_sig,
                    )
                    .await?;
                anyhow::ensure!(signed, "enrollment decision signature is invalid");
                Ok::<_, anyhow::Error>(VerifiedDecision {
                    doc_id: row.doc_id,
                    record,
                    signed,
                })
            }
            .await;
            match verified {
                Ok(decision) => decisions.push(decision),
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    }
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        format!("invalid enrollment decision: {error:#}"),
                    );
                    tracing::warn!(?scope, request_id, error = %error, "scoped invalid enrollment decision");
                }
            }
        }
        let decisions_by_request = decisions.iter().fold(BTreeMap::new(), |mut map, verified| {
            map.entry(verified.record.request_id.clone())
                .or_insert_with(Vec::new)
                .push(verified);
            map
        });
        for (request_id, rows) in &decisions_by_request {
            if rows.len() != 1 {
                add_request_conflict(
                    &mut request_conflicts,
                    request_id,
                    format!("request has {} immutable terminal decisions", rows.len()),
                );
            }
        }

        let mut revisions = Vec::new();
        for raw in rows::<Value>(response, "NetworkAuthorizationRevision")? {
            let request_id = raw_text(&raw, "request_id").map(str::to_string);
            let request_key = request_id.as_deref().unwrap_or("");
            let Some(scope) = scope_for_authority_row(
                &raw,
                request_key,
                &request_scopes,
                &network.network_id,
                "member_did",
            )?
            else {
                continue;
            };
            let observed_sequence =
                raw_i64(&raw, "sequence").and_then(|value| u64::try_from(value).ok());
            if let Some(sequence) = observed_sequence {
                observe_next_authorization_sequence(
                    &mut observed_revision_next,
                    &mut scoped_conflicts,
                    scope.clone(),
                    sequence,
                );
            }
            let Some(request_id) = request_id else {
                if let Some(sequence) = observed_sequence {
                    invalid_authority_max
                        .entry(scope)
                        .and_modify(|current| *current = (*current).max(sequence))
                        .or_insert(sequence);
                } else {
                    add_scope_conflict(
                        &mut scoped_conflicts,
                        scope,
                        "malformed authorization revision has no request_id or usable sequence",
                    );
                }
                continue;
            };
            let row = match serde_json::from_value::<RevisionRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    } else {
                        add_scope_conflict(
                            &mut scoped_conflicts,
                            scope,
                            format!(
                                "malformed authorization revision without usable sequence: {error}"
                            ),
                        );
                    }
                    continue;
                }
            };
            let verified = async {
                let record = row.to_record()?;
                let request = unique_request(&requests_by_id, &record.request_id)?;
                let decision = unique_approved_decision(&decisions_by_request, &record.request_id)?;
                record.validate_against_approval(&request.record, &decision.record)?;
                let signed = self
                    .identity
                    .verify(
                        &record.signer_did,
                        &record.signing_payload(),
                        &record.admin_sig,
                    )
                    .await?;
                anyhow::ensure!(signed, "authorization revision signature is invalid");
                Ok::<_, anyhow::Error>(VerifiedRevision {
                    doc_id: row.doc_id,
                    record,
                    signed,
                })
            }
            .await;
            match verified {
                Ok(revision) => revisions.push(revision),
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    } else {
                        add_scope_conflict(
                            &mut scoped_conflicts,
                            scope.clone(),
                            "invalid authorization revision without usable sequence",
                        );
                    }
                    tracing::warn!(?scope, request_id, error = %error, "scoped invalid authorization revision");
                }
            }
        }

        let valid_revision_max = revisions
            .iter()
            .fold(BTreeMap::new(), |mut maxes, revision| {
                let scope = (
                    revision.record.network_id.clone(),
                    revision.record.member_did.clone(),
                );
                maxes
                    .entry(scope)
                    .and_modify(|current: &mut u64| {
                        *current = (*current).max(revision.record.sequence)
                    })
                    .or_insert(revision.record.sequence);
                maxes
            });
        for (scope, invalid_max) in invalid_authority_max {
            if invalid_revision_is_current(valid_revision_max.get(&scope).copied(), invalid_max) {
                add_scope_conflict(
                    &mut scoped_conflicts,
                    scope,
                    format!("invalid authorization revision at observed sequence {invalid_max}"),
                );
            }
        }

        let mut receipts = Vec::new();
        let mut invalid_receipt_generations = BTreeSet::new();
        for raw in rows::<Value>(response, "NetworkEnrollmentRouteReceipt")? {
            let request_id = raw_text(&raw, "request_id").map(str::to_string);
            let request_key = request_id.as_deref().unwrap_or("");
            let Some(scope) = scope_for_authority_row(
                &raw,
                request_key,
                &request_scopes,
                &network.network_id,
                "member_did",
            )?
            else {
                continue;
            };
            let sequence =
                raw_i64(&raw, "authorization_sequence").and_then(|value| u64::try_from(value).ok());
            let Some(request_id) = request_id else {
                if let Some(sequence) = sequence {
                    invalid_receipt_generations.insert((scope, sequence));
                } else {
                    add_scope_conflict(
                        &mut scoped_conflicts,
                        scope,
                        "malformed route receipt has no request_id or usable generation",
                    );
                }
                continue;
            };
            let row = match serde_json::from_value::<RouteReceiptRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    if let Some(sequence) = sequence {
                        invalid_receipt_generations.insert((scope, sequence));
                    } else {
                        add_scope_conflict(
                            &mut scoped_conflicts,
                            scope,
                            format!("malformed route receipt without usable generation: {error}"),
                        );
                    }
                    continue;
                }
            };
            let verified = async {
                let record = row.to_record()?;
                let request = unique_request(&requests_by_id, &record.request_id)?;
                let decision = unique_approved_decision(&decisions_by_request, &record.request_id)?;
                record.validate_against_approval(&request.record, &decision.record)?;
                let signed = self
                    .identity
                    .verify(
                        &record.signer_did,
                        &record.signing_payload(),
                        &record.admin_sig,
                    )
                    .await?;
                anyhow::ensure!(signed, "enrollment route receipt signature is invalid");
                Ok::<_, anyhow::Error>(VerifiedRouteReceipt {
                    doc_id: row.doc_id,
                    record,
                })
            }
            .await;
            match verified {
                Ok(receipt) => receipts.push(receipt),
                Err(error) => {
                    if let Some(sequence) = sequence {
                        invalid_receipt_generations.insert((scope.clone(), sequence));
                    } else {
                        add_scope_conflict(
                            &mut scoped_conflicts,
                            scope.clone(),
                            "invalid route receipt without usable generation",
                        );
                    }
                    tracing::warn!(?scope, request_id, error = %error, "scoped invalid enrollment route receipt");
                }
            }
        }

        let mut durable = DurableEnrollmentDocuments {
            admin_pins: BTreeSet::from([NetworkAdminPin {
                network_id: network.network_id.clone(),
                admin_did: network.admin_did.clone(),
            }]),
            ..DurableEnrollmentDocuments::default()
        };
        for verified in &requests {
            durable.offers.insert(verified.offer.clone());
            durable.requests.insert(
                verified.pure_request(
                    now,
                    decisions_by_request
                        .get(&verified.record.request_id)
                        .is_some_and(|rows| rows.iter().any(|decision| decision.signed)),
                ),
            );
        }
        for verified in &decisions {
            durable.decisions.insert(verified.pure(now));
        }
        for verified in &revisions {
            durable.revisions.insert(verified.pure());
        }
        for verified in &receipts {
            durable.route_receipts.insert(verified.pure());
        }
        for receipt in &receipts {
            if !durable.route_receipt_identity_unique(&receipt.pure()) {
                invalid_receipt_generations.insert((
                    (
                        receipt.record.network_id.clone(),
                        receipt.record.member_did.clone(),
                    ),
                    receipt.record.authorization_sequence,
                ));
            }
        }
        for (scope, invalid_sequence) in &invalid_receipt_generations {
            if valid_revision_max
                .get(scope)
                .is_some_and(|valid_sequence| valid_sequence == invalid_sequence)
            {
                add_scope_conflict(
                    &mut scoped_conflicts,
                    scope.clone(),
                    format!(
                        "invalid enrollment route receipt at authorization sequence {invalid_sequence}"
                    ),
                );
            }
        }

        let mut projection = EnrollmentProjection {
            network_id: Some(network.network_id),
            next_authorization_sequences: observed_revision_next,
            ..EnrollmentProjection::default()
        };
        for verified in &requests {
            if request_conflicts.contains_key(&verified.record.request_id) {
                continue;
            }
            let matching_decisions = decisions_by_request
                .get(&verified.record.request_id)
                .cloned()
                .unwrap_or_default();
            if matching_decisions.is_empty() {
                if verified.is_fresh(now)? {
                    projection.pending.push(PendingEnrollment {
                        request_doc_id: verified.doc_id.clone(),
                        offer_token: verified.record.offer_token.clone(),
                        request: verified.record.clone(),
                    });
                }
                continue;
            }
            for decision in matching_decisions {
                if decision.record.decision == WireDecisionKind::Denied && decision.signed {
                    projection.denied.push(DeniedEnrollment {
                        request_doc_id: verified.doc_id.clone(),
                        decision_doc_id: decision.doc_id.clone(),
                        request: verified.record.clone(),
                        decision: decision.record.clone(),
                    });
                    continue;
                }
                let pure_request = verified.pure_request(now, decision.signed);
                let pure_decision = decision.pure(now);
                let Some(revision_rows) = revisions_for(&revisions, &verified.record.request_id)
                else {
                    continue;
                };
                for revision in revision_rows {
                    let scope = (
                        verified.record.network_id.clone(),
                        verified.record.candidate_did.clone(),
                    );
                    if scoped_conflicts.contains_key(&scope) {
                        continue;
                    }
                    if durable.current_approval(&verified.offer, &pure_request, &pure_decision)
                        && revision.record.kind == WireRevisionKind::Active
                        && revision.record.sequence == decision.record.authorization_sequence
                    {
                        let current_receipts = receipts
                            .iter()
                            .filter(|receipt| {
                                durable.current_server_route_receipt(
                                    &verified.offer,
                                    &pure_request,
                                    &pure_decision,
                                    &receipt.pure(),
                                )
                            })
                            .collect::<Vec<_>>();
                        let current_receipt = if invalid_receipt_generations
                            .contains(&(scope, decision.record.authorization_sequence))
                            || current_receipts.len() > 1
                        {
                            None
                        } else {
                            current_receipts.first().copied()
                        };
                        projection.active.push(ActiveEnrollment {
                            request_doc_id: verified.doc_id.clone(),
                            decision_doc_id: decision.doc_id.clone(),
                            revision_doc_id: revision.doc_id.clone(),
                            route_receipt_doc_id: current_receipt
                                .map(|receipt| receipt.doc_id.clone()),
                            request: verified.record.clone(),
                            decision: decision.record.clone(),
                            revision: revision.record.clone(),
                            route_receipt: current_receipt.map(|receipt| receipt.record.clone()),
                        });
                    }
                }
            }
        }
        // Retain the exact unique maximal tombstone even though it is not
        // operationally active. This is the durable revocation delivery owner.
        for verified in &requests {
            if request_conflicts.contains_key(&verified.record.request_id) {
                continue;
            }
            let scope = (
                verified.record.network_id.as_str(),
                verified.record.candidate_did.as_str(),
            );
            if scoped_conflicts.contains_key(&(scope.0.to_string(), scope.1.to_string())) {
                continue;
            }
            let scoped = revisions
                .iter()
                .filter(|revision| {
                    revision.record.network_id == scope.0 && revision.record.member_did == scope.1
                })
                .collect::<Vec<_>>();
            let Some(max_sequence) = scoped.iter().map(|row| row.record.sequence).max() else {
                continue;
            };
            let maxima = scoped
                .into_iter()
                .filter(|row| row.record.sequence == max_sequence)
                .collect::<Vec<_>>();
            let [revision] = maxima.as_slice() else {
                continue;
            };
            if revision.record.kind != WireRevisionKind::Revoked
                || revision.record.request_id != verified.record.request_id
            {
                continue;
            }
            let Some([decision]) = decisions_by_request
                .get(&verified.record.request_id)
                .map(Vec::as_slice)
            else {
                continue;
            };
            if decision.record.decision != WireDecisionKind::Approved {
                continue;
            }
            projection.revoked.push(RevokedEnrollment {
                request_doc_id: verified.doc_id.clone(),
                decision_doc_id: decision.doc_id.clone(),
                revision_doc_id: revision.doc_id.clone(),
                request: verified.record.clone(),
                decision: decision.record.clone(),
                revision: revision.record.clone(),
            });
        }
        projection
            .pending
            .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
        projection
            .active
            .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
        let mut collision_scopes = BTreeSet::new();
        for (index, active) in projection.active.iter().enumerate() {
            for other in &projection.active[index + 1..] {
                if active.request.candidate_peer == other.request.candidate_peer
                    && (active.request.candidate_did != other.request.candidate_did
                        || active.request.owner_agent != other.request.owner_agent)
                {
                    collision_scopes.insert((
                        active.request.network_id.clone(),
                        active.request.candidate_did.clone(),
                    ));
                    collision_scopes.insert((
                        other.request.network_id.clone(),
                        other.request.candidate_did.clone(),
                    ));
                }
            }
        }
        for scope in collision_scopes {
            add_scope_conflict(
                &mut scoped_conflicts,
                scope,
                "current enrollment routes collide on one transport peer",
            );
        }
        projection.active.retain(|active| {
            !scoped_conflicts.contains_key(&(
                active.request.network_id.clone(),
                active.request.candidate_did.clone(),
            ))
        });
        projection
            .denied
            .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
        projection
            .revoked
            .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
        projection.scoped_conflicts = finish_conflicts(scoped_conflicts);
        projection.request_conflicts = finish_conflicts(request_conflicts);
        Ok(projection)
    }

    async fn verify_request(
        &self,
        row: RequestRow,
        network: &NetworkRecord,
    ) -> Result<VerifiedRequest> {
        let record = row.to_record()?;
        let offer = decode_offer(&record.offer_token)?;
        anyhow::ensure!(
            offer.schema_fingerprint == enrollment_schema_fingerprint(),
            "enrollment offer schema fingerprint mismatch"
        );
        anyhow::ensure!(
            offer.network_id == network.network_id,
            "offer network mismatch"
        );
        anyhow::ensure!(offer.admin_did == network.admin_did, "offer admin mismatch");
        record.validate_against_offer(&offer)?;
        let (server_ticket_peer, _) = parse_public_peer_addr(&offer.server_ticket)
            .context("parsing enrollment server ticket")?;
        let (candidate_ticket_peer, _) = parse_public_peer_addr(&record.candidate_ticket)
            .context("parsing enrollment candidate ticket")?;
        anyhow::ensure!(
            server_ticket_peer.to_string() == offer.server_peer,
            "offer server ticket peer mismatch"
        );
        anyhow::ensure!(
            candidate_ticket_peer.to_string() == record.candidate_peer,
            "request candidate ticket peer mismatch"
        );
        let offer_signed = self
            .identity
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?;
        let candidate_signed = self
            .identity
            .verify(
                &record.candidate_did,
                &record.signing_payload(),
                &record.candidate_sig,
            )
            .await?;
        Ok(VerifiedRequest {
            doc_id: row.doc_id,
            record,
            offer: EnrollmentOffer {
                offer_id: offer.offer_id,
                challenge: offer.challenge,
                network_id: offer.network_id,
                admin_did: offer.admin_did.clone(),
                server_peer: offer.server_peer.clone(),
                server_ticket_peer: server_ticket_peer.to_string(),
                resolved_server_did: offer.admin_did,
                owner_agent: offer.owner_agent,
                profile: offer.profile,
                schema_compatible: offer.schema_fingerprint == enrollment_schema_fingerprint(),
                admin_signed: offer_signed,
                fresh: true,
            },
            candidate_signed,
        })
    }
}

fn required_raw_text<'a>(raw: &'a Value, field: &str, row_kind: &str) -> Result<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("unattributable {row_kind} has no {field}"))
}

fn raw_text<'a>(raw: &'a Value, field: &str) -> Option<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
enum CandidateRequestAttribution {
    Exact {
        request_id: String,
        scope: (String, String),
    },
    RequestOnly(String),
    ScopeOnly((String, String)),
    ForeignOrUnattributable,
}

fn attribute_candidate_request(raw: &Value, local_network: &str) -> CandidateRequestAttribution {
    let network_id = raw_text(raw, "network_id");
    if network_id.is_some_and(|network_id| network_id != local_network) {
        return CandidateRequestAttribution::ForeignOrUnattributable;
    }
    // This store has one cryptographically verified local AgentNetwork root.
    // A row missing only its network field remains attributable to that root
    // by request/member identity and must quarantine that narrow scope.
    let network_id = network_id.unwrap_or(local_network);
    let request_id = raw_text(raw, "request_id").map(str::to_string);
    let member_did = raw_text(raw, "candidate_did").map(str::to_string);
    match (request_id, member_did) {
        (Some(request_id), Some(member_did)) => CandidateRequestAttribution::Exact {
            request_id,
            scope: (network_id.to_string(), member_did),
        },
        (Some(request_id), None) => CandidateRequestAttribution::RequestOnly(request_id),
        (None, Some(member_did)) => {
            CandidateRequestAttribution::ScopeOnly((network_id.to_string(), member_did))
        }
        (None, None) => CandidateRequestAttribution::ForeignOrUnattributable,
    }
}

fn raw_i64(raw: &Value, field: &str) -> Option<i64> {
    raw.get(field)?.as_i64()
}

fn scope_for_authority_row(
    raw: &Value,
    request_id: &str,
    request_scopes: &BTreeMap<String, (String, String)>,
    local_network_id: &str,
    member_field: &str,
) -> Result<Option<(String, String)>> {
    if let Some(scope) = request_scopes.get(request_id) {
        return Ok(Some(scope.clone()));
    }
    let network_id = raw_text(raw, "network_id");
    if network_id.is_some_and(|network_id| network_id != local_network_id) {
        return Ok(None);
    }
    let member_did = required_raw_text(raw, member_field, "enrollment authority row")?;
    Ok(Some((
        network_id.unwrap_or(local_network_id).to_string(),
        member_did.to_string(),
    )))
}

fn add_scope_conflict(
    conflicts: &mut BTreeMap<(String, String), Vec<String>>,
    scope: (String, String),
    reason: impl Into<String>,
) {
    add_conflict(conflicts, scope, reason);
}

fn add_request_conflict(
    conflicts: &mut BTreeMap<String, Vec<String>>,
    request_id: &str,
    reason: impl Into<String>,
) {
    add_conflict(conflicts, request_id.to_string(), reason);
}

fn add_conflict<K: Ord>(
    conflicts: &mut BTreeMap<K, Vec<String>>,
    key: K,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    let reasons = conflicts.entry(key).or_default();
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn finish_conflicts<K: Ord>(conflicts: BTreeMap<K, Vec<String>>) -> BTreeMap<K, String> {
    conflicts
        .into_iter()
        .map(|(key, reasons)| (key, reasons.join("; ")))
        .collect()
}

fn invalid_revision_is_current(valid_max: Option<u64>, invalid_max: u64) -> bool {
    valid_max.unwrap_or_default() <= invalid_max
}

fn observe_next_authorization_sequence(
    next_by_scope: &mut BTreeMap<(String, String), u64>,
    conflicts: &mut BTreeMap<(String, String), Vec<String>>,
    scope: (String, String),
    observed: u64,
) {
    let Some(next) = observed.checked_add(1) else {
        add_scope_conflict(
            conflicts,
            scope,
            "authorization sequence exhausted the DefraDB Int range",
        );
        return;
    };
    if next > i64::MAX as u64 {
        add_scope_conflict(
            conflicts,
            scope.clone(),
            "authorization sequence exhausted the DefraDB Int range",
        );
    }
    next_by_scope
        .entry(scope)
        .and_modify(|current| *current = (*current).max(next))
        .or_insert(next);
}

fn terminal_documents(
    decision_doc_id: &str,
    revision_doc_id: Option<&str>,
    route_receipt_doc_id: Option<&str>,
) -> Vec<P2pDocumentRequest> {
    let mut documents = vec![P2pDocumentRequest {
        collection: "NetworkEnrollmentDecision".to_string(),
        doc_id: decision_doc_id.to_string(),
    }];
    if let Some(revision_doc_id) = revision_doc_id {
        documents.push(P2pDocumentRequest {
            collection: "NetworkAuthorizationRevision".to_string(),
            doc_id: revision_doc_id.to_string(),
        });
    }
    if let Some(route_receipt_doc_id) = route_receipt_doc_id {
        documents.push(P2pDocumentRequest {
            collection: "NetworkEnrollmentRouteReceipt".to_string(),
            doc_id: route_receipt_doc_id.to_string(),
        });
    }
    documents
}

fn terminal_is_exact_current(
    projection: &EnrollmentProjection,
    request: &EnrollmentRequestRecord,
    decision_doc_id: &str,
    revision_doc_id: Option<&str>,
    route_receipt_doc_id: Option<&str>,
) -> bool {
    projection.active.iter().any(|active| {
        active.request == *request
            && active.decision_doc_id == decision_doc_id
            && revision_doc_id == Some(active.revision_doc_id.as_str())
            && route_receipt_doc_id == active.route_receipt_doc_id.as_deref()
    }) || projection.denied.iter().any(|denied| {
        denied.request == *request
            && denied.decision_doc_id == decision_doc_id
            && revision_doc_id.is_none()
            && route_receipt_doc_id.is_none()
    }) || projection.revoked.iter().any(|revoked| {
        revoked.request == *request
            && revoked.decision_doc_id == decision_doc_id
            && revision_doc_id == Some(revoked.revision_doc_id.as_str())
            && route_receipt_doc_id.is_none()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentDecisionOutcome {
    pub request_id: String,
    pub state: &'static str,
    pub decision_doc_id: String,
    pub revision_doc_id: Option<String>,
    pub delivery_pending: bool,
}

#[derive(Debug)]
struct VerifiedRequest {
    doc_id: String,
    record: EnrollmentRequestRecord,
    offer: EnrollmentOffer,
    candidate_signed: bool,
}

impl VerifiedRequest {
    fn is_fresh(&self, now: DateTime<Utc>) -> Result<bool> {
        let issued_at = DateTime::parse_from_rfc3339(&self.record.issued_at)?.with_timezone(&Utc);
        let expires_at = DateTime::parse_from_rfc3339(&self.record.expires_at)?.with_timezone(&Utc);
        Ok(issued_at <= now && now <= expires_at)
    }

    fn pure_request(&self, now: DateTime<Utc>, terminal_witness: bool) -> EnrollmentRequest {
        EnrollmentRequest {
            request_id: self.record.request_id.clone(),
            digest: self.record.request_digest.clone(),
            offer_id: self.record.offer_id.clone(),
            challenge: self.record.challenge.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            server_peer: self.record.server_peer.clone(),
            candidate_did: self.record.candidate_did.clone(),
            candidate_peer: self.record.candidate_peer.clone(),
            observed_candidate_peer: terminal_witness
                .then(|| self.record.candidate_peer.clone())
                .unwrap_or_default(),
            resolved_candidate_did: terminal_witness
                .then(|| self.record.candidate_did.clone())
                .unwrap_or_default(),
            candidate_ticket_peer: self.record.candidate_peer.clone(),
            owner_agent: self.record.owner_agent.clone(),
            profile: self.record.profile.clone(),
            client_nonce: self.record.client_nonce.clone(),
            issued_at: self.record.issued_at.clone(),
            expires_at: self.record.expires_at.clone(),
            candidate_signed: self.candidate_signed,
            fresh: terminal_witness || self.is_fresh(now).unwrap_or(false),
        }
    }
}

#[derive(Debug)]
struct VerifiedDecision {
    doc_id: String,
    record: EnrollmentDecisionRecord,
    signed: bool,
}

impl VerifiedDecision {
    fn pure(&self, now: DateTime<Utc>) -> EnrollmentDecision {
        let fresh = self.record.decision == WireDecisionKind::Denied
            || DateTime::parse_from_rfc3339(&self.record.authorization_expires_at)
                .map(|expires| now < expires.with_timezone(&Utc))
                .unwrap_or(false);
        EnrollmentDecision {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            candidate_did: self.record.candidate_did.clone(),
            candidate_peer: self.record.candidate_peer.clone(),
            owner_agent: self.record.owner_agent.clone(),
            kind: match self.record.decision {
                WireDecisionKind::Approved => EnrollmentDecisionKind::Approved,
                WireDecisionKind::Denied => EnrollmentDecisionKind::Denied,
            },
            authorization_sequence: self.record.authorization_sequence as usize,
            authorization_expires_at: self.record.authorization_expires_at.clone(),
            signer_did: self.record.signer_did.clone(),
            admin_signed: self.signed,
            fresh,
        }
    }
}

#[derive(Debug)]
struct VerifiedRevision {
    doc_id: String,
    record: AuthorizationRevisionRecord,
    signed: bool,
}

#[derive(Debug)]
struct VerifiedRouteReceipt {
    doc_id: String,
    record: EnrollmentRouteReceiptRecord,
}

impl VerifiedRouteReceipt {
    fn pure(&self) -> EnrollmentRouteReceipt {
        EnrollmentRouteReceipt {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            member_did: self.record.member_did.clone(),
            member_peer: self.record.member_peer.clone(),
            server_peer: self.record.server_peer.clone(),
            owner_agent: self.record.owner_agent.clone(),
            authorization_sequence: self.record.authorization_sequence as usize,
            authorization_expires_at: self.record.authorization_expires_at.clone(),
            direction: EnrollmentRouteDirection::ClientToServer,
            signer_did: self.record.signer_did.clone(),
            admin_signed: true,
            applied: true,
        }
    }
}

impl VerifiedRevision {
    fn pure(&self) -> AuthorizationRevision {
        AuthorizationRevision {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            member_did: self.record.member_did.clone(),
            member_peer: self.record.member_peer.clone(),
            owner_agent: self.record.owner_agent.clone(),
            sequence: self.record.sequence as usize,
            authorization_expires_at: self.record.authorization_expires_at.clone(),
            kind: match self.record.kind {
                WireRevisionKind::Active => AuthorizationRevisionKind::Active,
                WireRevisionKind::Revoked => AuthorizationRevisionKind::Revoked,
            },
            signer_did: self.record.signer_did.clone(),
            admin_signed: self.signed,
        }
    }
}

fn unique_request<'a>(
    rows: &'a BTreeMap<String, Vec<&VerifiedRequest>>,
    request_id: &str,
) -> Result<&'a VerifiedRequest> {
    match rows.get(request_id).map(Vec::as_slice) {
        Some([request]) => Ok(*request),
        Some(rows) => anyhow::bail!("request {request_id} has {} conflicting rows", rows.len()),
        None => anyhow::bail!("request {request_id} is missing"),
    }
}

fn unique_approved_decision<'a>(
    rows: &'a BTreeMap<String, Vec<&VerifiedDecision>>,
    request_id: &str,
) -> Result<&'a VerifiedDecision> {
    match rows.get(request_id).map(Vec::as_slice) {
        Some([decision]) if decision.record.decision == WireDecisionKind::Approved => Ok(*decision),
        Some(rows) => anyhow::bail!(
            "request {request_id} has no unique approval ({} terminal rows)",
            rows.len()
        ),
        None => anyhow::bail!("request {request_id} has no approval"),
    }
}

fn revisions_for<'a>(
    rows: &'a [VerifiedRevision],
    request_id: &str,
) -> Option<Vec<&'a VerifiedRevision>> {
    let selected = rows
        .iter()
        .filter(|row| row.record.request_id == request_id)
        .collect::<Vec<_>>();
    (!selected.is_empty()).then_some(selected)
}

fn decision_mutation(
    decision: &EnrollmentDecisionRecord,
    revision: Option<&AuthorizationRevisionRecord>,
) -> String {
    let escaped = |value: &str| crate::graphql::escape_graphql_string(value);
    let decision_sig = bs58::encode(&decision.admin_sig).into_string();
    let decision_input = format!(
        r#"{{
          protocol_version: {}, decision_id: "{}", request_id: "{}",
          request_digest: "{}", network_id: "{}", admin_did: "{}",
          candidate_did: "{}", candidate_peer: "{}", owner_agent: "{}",
          decision: "{}", authorization_sequence: {}, authorization_expires_at: "{}", decided_at: "{}",
          signer_did: "{}", admin_sig: "{}"
        }}"#,
        decision.protocol_version,
        escaped(&decision.decision_id),
        escaped(&decision.request_id),
        escaped(&decision.request_digest),
        escaped(&decision.network_id),
        escaped(&decision.admin_did),
        escaped(&decision.candidate_did),
        escaped(&decision.candidate_peer),
        escaped(&decision.owner_agent),
        decision.decision.as_str(),
        decision.authorization_sequence,
        escaped(&decision.authorization_expires_at),
        escaped(&decision.decided_at),
        escaped(&decision.signer_did),
        escaped(&decision_sig),
    );
    let revision_field = revision.map_or_else(String::new, |revision| {
        let revision_sig = bs58::encode(&revision.admin_sig).into_string();
        format!(
            r#"
              create_NetworkAuthorizationRevision(input: {{
                protocol_version: {}, revision_id: "{}", request_id: "{}",
                request_digest: "{}", network_id: "{}", admin_did: "{}",
                member_did: "{}", member_peer: "{}", owner_agent: "{}",
                sequence: {}, authorization_expires_at: "{}", kind: "{}", issued_at: "{}", signer_did: "{}",
                admin_sig: "{}"
              }}) {{ _docID }}"#,
            revision.protocol_version,
            escaped(&revision.revision_id),
            escaped(&revision.request_id),
            escaped(&revision.request_digest),
            escaped(&revision.network_id),
            escaped(&revision.admin_did),
            escaped(&revision.member_did),
            escaped(&revision.member_peer),
            escaped(&revision.owner_agent),
            revision.sequence,
            escaped(&revision.authorization_expires_at),
            revision.kind.as_str(),
            escaped(&revision.issued_at),
            escaped(&revision.signer_did),
            escaped(&revision_sig),
        )
    });
    format!(
        r#"mutation {{
          create_NetworkEnrollmentDecision(input: {decision_input}) {{ _docID }}
          {revision_field}
        }}"#
    )
}

fn revision_mutation(revision: &AuthorizationRevisionRecord) -> String {
    let escaped = |value: &str| crate::graphql::escape_graphql_string(value);
    let signature = bs58::encode(&revision.admin_sig).into_string();
    format!(
        r#"mutation {{
          create_NetworkAuthorizationRevision(input: {{
            protocol_version: {}, revision_id: "{}", request_id: "{}",
            request_digest: "{}", network_id: "{}", admin_did: "{}",
            member_did: "{}", member_peer: "{}", owner_agent: "{}",
            sequence: {}, authorization_expires_at: "{}", kind: "{}",
            issued_at: "{}", signer_did: "{}", admin_sig: "{}"
          }}) {{ _docID }}
        }}"#,
        revision.protocol_version,
        escaped(&revision.revision_id),
        escaped(&revision.request_id),
        escaped(&revision.request_digest),
        escaped(&revision.network_id),
        escaped(&revision.admin_did),
        escaped(&revision.member_did),
        escaped(&revision.member_peer),
        escaped(&revision.owner_agent),
        revision.sequence,
        escaped(&revision.authorization_expires_at),
        revision.kind.as_str(),
        escaped(&revision.issued_at),
        escaped(&revision.signer_did),
        escaped(&signature),
    )
}

fn route_receipt_mutation(receipt: &EnrollmentRouteReceiptRecord) -> String {
    let escaped = |value: &str| crate::graphql::escape_graphql_string(value);
    let admin_sig = bs58::encode(&receipt.admin_sig).into_string();
    format!(
        r#"mutation {{
          create_NetworkEnrollmentRouteReceipt(input: {{
            protocol_version: {}, receipt_id: "{}", request_id: "{}",
            request_digest: "{}", network_id: "{}", admin_did: "{}",
            member_did: "{}", member_peer: "{}", server_peer: "{}",
            owner_agent: "{}", authorization_sequence: {}, authorization_expires_at: "{}", direction: "{}",
            applied_at: "{}", signer_did: "{}", admin_sig: "{}"
          }}) {{ _docID }}
        }}"#,
        receipt.protocol_version,
        escaped(&receipt.receipt_id),
        escaped(&receipt.request_id),
        escaped(&receipt.request_digest),
        escaped(&receipt.network_id),
        escaped(&receipt.admin_did),
        escaped(&receipt.member_did),
        escaped(&receipt.member_peer),
        escaped(&receipt.server_peer),
        escaped(&receipt.owner_agent),
        receipt.authorization_sequence,
        escaped(&receipt.authorization_expires_at),
        receipt.direction.as_str(),
        escaped(&receipt.applied_at),
        escaped(&receipt.signer_did),
        escaped(&admin_sig),
    )
}

#[derive(Debug, Deserialize)]
struct AgentNetworkRow {
    network_id: String,
    admin_did: String,
    display_name: String,
    default_template: String,
    created_at: String,
    admin_sig: String,
}

impl AgentNetworkRow {
    fn to_record(&self) -> Result<NetworkRecord> {
        Ok(NetworkRecord {
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            display_name: self.display_name.clone(),
            default_template: self.default_template.clone(),
            created_at: self.created_at.clone(),
            sig: decode_signature("AgentNetwork.admin_sig", &self.admin_sig)?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    protocol_version: i64,
    request_id: String,
    request_digest: String,
    offer_id: String,
    offer_token: String,
    challenge: String,
    network_id: String,
    admin_did: String,
    server_peer: String,
    candidate_did: String,
    candidate_peer: String,
    candidate_ticket: String,
    owner_agent: String,
    profile: String,
    client_nonce: String,
    issued_at: String,
    expires_at: String,
    candidate_sig: String,
}

impl RequestRow {
    fn to_record(&self) -> Result<EnrollmentRequestRecord> {
        Ok(EnrollmentRequestRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            offer_id: self.offer_id.clone(),
            offer_token: self.offer_token.clone(),
            challenge: self.challenge.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            server_peer: self.server_peer.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            candidate_ticket: self.candidate_ticket.clone(),
            owner_agent: self.owner_agent.clone(),
            profile: self.profile.clone(),
            client_nonce: self.client_nonce.clone(),
            issued_at: self.issued_at.clone(),
            expires_at: self.expires_at.clone(),
            candidate_sig: decode_signature(
                "NetworkEnrollmentRequest.candidate_sig",
                &self.candidate_sig,
            )?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct DecisionRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    protocol_version: i64,
    decision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    candidate_did: String,
    candidate_peer: String,
    owner_agent: String,
    decision: String,
    authorization_sequence: i64,
    authorization_expires_at: String,
    decided_at: String,
    signer_did: String,
    admin_sig: String,
}

impl DecisionRow {
    fn to_record(&self) -> Result<EnrollmentDecisionRecord> {
        Ok(EnrollmentDecisionRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            decision_id: self.decision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            decision: match self.decision.as_str() {
                "approved" => WireDecisionKind::Approved,
                "denied" => WireDecisionKind::Denied,
                other => anyhow::bail!("unknown enrollment decision {other:?}"),
            },
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative enrollment authorization sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            decided_at: self.decided_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: decode_signature("NetworkEnrollmentDecision.admin_sig", &self.admin_sig)?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RevisionRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    protocol_version: i64,
    revision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    member_did: String,
    member_peer: String,
    owner_agent: String,
    sequence: i64,
    authorization_expires_at: String,
    kind: String,
    issued_at: String,
    signer_did: String,
    admin_sig: String,
}

#[derive(Debug, Deserialize)]
struct RouteReceiptRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    protocol_version: i64,
    receipt_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    member_did: String,
    member_peer: String,
    server_peer: String,
    owner_agent: String,
    authorization_sequence: i64,
    authorization_expires_at: String,
    direction: String,
    applied_at: String,
    signer_did: String,
    admin_sig: String,
}

impl RouteReceiptRow {
    fn to_record(&self) -> Result<EnrollmentRouteReceiptRecord> {
        Ok(EnrollmentRouteReceiptRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            receipt_id: self.receipt_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            server_peer: self.server_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative route receipt authorization sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            direction: match self.direction.as_str() {
                "client_to_server" => WireReceiptDirection::ClientToServer,
                other => anyhow::bail!("unknown enrollment route receipt direction {other:?}"),
            },
            applied_at: self.applied_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: decode_signature(
                "NetworkEnrollmentRouteReceipt.admin_sig",
                &self.admin_sig,
            )?,
        })
    }
}

impl RevisionRow {
    fn to_record(&self) -> Result<AuthorizationRevisionRecord> {
        Ok(AuthorizationRevisionRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            revision_id: self.revision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            sequence: u64::try_from(self.sequence).context("negative authorization sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            kind: match self.kind.as_str() {
                "active" => WireRevisionKind::Active,
                "revoked" => WireRevisionKind::Revoked,
                other => anyhow::bail!("unknown authorization revision kind {other:?}"),
            },
            issued_at: self.issued_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: decode_signature("NetworkAuthorizationRevision.admin_sig", &self.admin_sig)?,
        })
    }
}

fn parse_protocol_version(value: i64) -> Result<u8> {
    let version = u8::try_from(value).context("invalid enrollment protocol version")?;
    anyhow::ensure!(
        version == ENROLLMENT_PROTOCOL_VERSION,
        "unsupported enrollment protocol version {version}"
    );
    Ok(version)
}

fn decode_signature(field: &str, value: &str) -> Result<Vec<u8>> {
    let signature = bs58::decode(value)
        .into_vec()
        .with_context(|| format!("decoding {field}"))?;
    anyhow::ensure!(
        signature.len() == 64,
        "{field} must contain a 64-byte signature"
    );
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_request(request_id: &str) -> PendingEnrollment {
        PendingEnrollment {
            request_doc_id: format!("doc-{request_id}"),
            offer_token: "offer".into(),
            request: EnrollmentRequestRecord {
                protocol_version: ENROLLMENT_PROTOCOL_VERSION,
                request_id: request_id.into(),
                request_digest: format!("digest-{request_id}"),
                offer_id: "offer".into(),
                offer_token: "offer".into(),
                challenge: format!("challenge-{request_id}"),
                network_id: "network-1".into(),
                admin_did: "did:key:admin".into(),
                server_peer: "server-peer".into(),
                candidate_did: "did:key:member-a".into(),
                candidate_peer: "member-peer-a".into(),
                candidate_ticket: "ticket".into(),
                owner_agent: "did:key:owner".into(),
                profile: "client".into(),
                client_nonce: "nonce".into(),
                issued_at: "2026-08-29T12:00:00Z".into(),
                expires_at: "2026-08-29T12:05:00Z".into(),
                candidate_sig: vec![1; 64],
            },
        }
    }

    fn decision(kind: WireDecisionKind) -> EnrollmentDecisionRecord {
        let approved = kind == WireDecisionKind::Approved;
        EnrollmentDecisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            decision_id: "decision-\"\\\n".to_string(),
            request_id: "request-1".to_string(),
            request_digest: "digest-1".to_string(),
            network_id: "network-1".to_string(),
            admin_did: "did:key:admin".to_string(),
            candidate_did: "did:key:candidate".to_string(),
            candidate_peer: "peer-1".to_string(),
            owner_agent: "did:key:owner".to_string(),
            decision: kind,
            authorization_sequence: if approved { 7 } else { 0 },
            authorization_expires_at: if approved {
                "2026-09-28T12:00:00Z".to_string()
            } else {
                "2026-08-29T12:00:00Z".to_string()
            },
            decided_at: "2026-08-29T12:00:00Z".to_string(),
            signer_did: "did:key:admin".to_string(),
            admin_sig: vec![1; 64],
        }
    }

    fn revision() -> AuthorizationRevisionRecord {
        AuthorizationRevisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            revision_id: "revision-1".to_string(),
            request_id: "request-1".to_string(),
            request_digest: "digest-1".to_string(),
            network_id: "network-1".to_string(),
            admin_did: "did:key:admin".to_string(),
            member_did: "did:key:candidate".to_string(),
            member_peer: "peer-1".to_string(),
            owner_agent: "did:key:owner".to_string(),
            sequence: 7,
            authorization_expires_at: "2026-09-28T12:00:00Z".to_string(),
            kind: WireRevisionKind::Active,
            issued_at: "2026-08-29T12:00:00Z".to_string(),
            signer_did: "did:key:admin".to_string(),
            admin_sig: vec![2; 64],
        }
    }

    fn receipt() -> EnrollmentRouteReceiptRecord {
        EnrollmentRouteReceiptRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            receipt_id: "receipt-\"\\\n".to_string(),
            request_id: "request-1".to_string(),
            request_digest: "digest-1".to_string(),
            network_id: "network-1".to_string(),
            admin_did: "did:key:admin".to_string(),
            member_did: "did:key:candidate".to_string(),
            member_peer: "peer-1".to_string(),
            server_peer: "server-peer".to_string(),
            owner_agent: "did:key:owner".to_string(),
            authorization_sequence: 7,
            authorization_expires_at: "2026-09-28T12:00:00Z".to_string(),
            direction: WireReceiptDirection::ClientToServer,
            applied_at: "2026-08-29T12:00:01Z".to_string(),
            signer_did: "did:key:admin".to_string(),
            admin_sig: vec![3; 64],
        }
    }

    #[test]
    fn approval_mutation_is_atomic_and_graphql_escaped() {
        let mutation = decision_mutation(&decision(WireDecisionKind::Approved), Some(&revision()));

        assert_eq!(
            mutation.matches("create_NetworkEnrollmentDecision").count(),
            1
        );
        assert_eq!(
            mutation
                .matches("create_NetworkAuthorizationRevision")
                .count(),
            1
        );
        let escaped = crate::graphql::escape_graphql_string("decision-\"\\\n");
        assert!(mutation.contains(&format!(r#"decision_id: "{escaped}""#)));
        assert!(!mutation.contains("[]"));
    }

    #[test]
    fn denial_mutation_cannot_create_authority() {
        let denied = decision(WireDecisionKind::Denied);
        assert_eq!(denied.authorization_expires_at, denied.decided_at);
        let mutation = decision_mutation(&denied, None);

        assert_eq!(
            mutation.matches("create_NetworkEnrollmentDecision").count(),
            1
        );
        assert!(!mutation.contains("NetworkAuthorizationRevision"));
        assert!(!mutation.contains("[]"));
    }

    #[test]
    fn route_receipt_mutation_is_immutable_bounded_and_escaped() {
        let mutation = route_receipt_mutation(&receipt());
        assert_eq!(
            mutation
                .matches("create_NetworkEnrollmentRouteReceipt")
                .count(),
            1
        );
        let escaped = crate::graphql::escape_graphql_string("receipt-\"\\\n");
        assert!(mutation.contains(&format!(r#"receipt_id: "{escaped}""#)));
        assert!(mutation.contains("direction: \"client_to_server\""));
        assert!(!mutation.contains("[]"));
    }

    #[test]
    fn approved_delivery_contains_exact_generation_receipt() {
        let documents =
            terminal_documents("decision-doc", Some("revision-doc"), Some("receipt-doc"));
        assert_eq!(documents.len(), 3);
        assert_eq!(documents[0].collection, "NetworkEnrollmentDecision");
        assert_eq!(documents[0].doc_id, "decision-doc");
        assert_eq!(documents[1].collection, "NetworkAuthorizationRevision");
        assert_eq!(documents[1].doc_id, "revision-doc");
        assert_eq!(documents[2].collection, "NetworkEnrollmentRouteReceipt");
        assert_eq!(documents[2].doc_id, "receipt-doc");
    }

    #[test]
    fn terminal_delivery_fence_suppresses_stale_active_after_revocation() {
        let pending = pending_request("request-1");
        let mut approved = decision(WireDecisionKind::Approved);
        approved.request_id = pending.request.request_id.clone();
        approved.request_digest = pending.request.request_digest.clone();
        approved.network_id = pending.request.network_id.clone();
        approved.admin_did = pending.request.admin_did.clone();
        approved.candidate_did = pending.request.candidate_did.clone();
        approved.candidate_peer = pending.request.candidate_peer.clone();
        approved.owner_agent = pending.request.owner_agent.clone();
        let mut revoked = revision();
        revoked.request_id = pending.request.request_id.clone();
        revoked.request_digest = pending.request.request_digest.clone();
        revoked.network_id = pending.request.network_id.clone();
        revoked.admin_did = pending.request.admin_did.clone();
        revoked.member_did = pending.request.candidate_did.clone();
        revoked.member_peer = pending.request.candidate_peer.clone();
        revoked.owner_agent = pending.request.owner_agent.clone();
        revoked.sequence = approved.authorization_sequence + 1;
        revoked.kind = WireRevisionKind::Revoked;

        let projection = EnrollmentProjection {
            revoked: vec![RevokedEnrollment {
                request_doc_id: pending.request_doc_id,
                decision_doc_id: "decision-doc".into(),
                revision_doc_id: "revoked-doc".into(),
                request: pending.request.clone(),
                decision: approved,
                revision: revoked,
            }],
            ..EnrollmentProjection::default()
        };
        assert!(!terminal_is_exact_current(
            &projection,
            &pending.request,
            "decision-doc",
            Some("active-doc"),
            Some("receipt-doc"),
        ));
        assert!(terminal_is_exact_current(
            &projection,
            &pending.request,
            "decision-doc",
            Some("revoked-doc"),
            None,
        ));
    }

    #[test]
    fn scoped_member_conflict_does_not_block_a_new_valid_request() {
        let mut projection = EnrollmentProjection {
            network_id: Some("network-1".into()),
            pending: vec![pending_request("request-new")],
            scoped_conflicts: BTreeMap::from([(
                ("network-1".into(), "did:key:member-a".into()),
                "hostile old revision".into(),
            )]),
            request_conflicts: BTreeMap::from([(
                "request-old".into(),
                "immutable terminal conflict".into(),
            )]),
            next_authorization_sequences: BTreeMap::from([(
                ("network-1".into(), "did:key:member-a".into()),
                10,
            )]),
            ..EnrollmentProjection::default()
        };
        assert_eq!(
            projection
                .pending_for_decision("request-new")
                .unwrap()
                .request
                .request_id,
            "request-new"
        );
        projection.pending.push(pending_request("request-old"));
        assert!(projection.pending_for_decision("request-old").is_err());
        assert_eq!(
            projection.next_authorization_sequences
                [&("network-1".into(), "did:key:member-a".into())],
            10
        );
    }

    #[test]
    fn newer_valid_revision_recovers_a_scoped_invalid_maximum() {
        assert!(invalid_revision_is_current(Some(8), 8));
        assert!(invalid_revision_is_current(Some(8), 9));
        assert!(!invalid_revision_is_current(Some(10), 9));

        let scope = ("network-1".to_string(), "did:key:member-a".to_string());
        let mut next = BTreeMap::new();
        let mut conflicts = BTreeMap::new();
        observe_next_authorization_sequence(&mut next, &mut conflicts, scope.clone(), 9);
        assert_eq!(next.get(&scope), Some(&10));
        assert!(conflicts.is_empty());
    }

    #[test]
    fn authority_row_attribution_is_member_scoped_and_root_corruption_is_global() {
        let scopes = BTreeMap::from([(
            "request-a".into(),
            ("network-1".into(), "did:key:member-a".into()),
        )]);
        let bad_a = json!({
            "request_id": "request-a",
            "network_id": "network-1",
            "member_did": "did:key:attacker"
        });
        assert_eq!(
            scope_for_authority_row(&bad_a, "request-a", &scopes, "network-1", "member_did",)
                .unwrap(),
            Some(("network-1".into(), "did:key:member-a".into()))
        );
        let foreign = json!({
            "request_id": "foreign",
            "network_id": "network-2",
            "member_did": "did:key:foreign"
        });
        assert!(
            scope_for_authority_row(&foreign, "foreign", &scopes, "network-1", "member_did",)
                .unwrap()
                .is_none()
        );
        let missing_network = json!({ "member_did": "did:key:member-a" });
        assert_eq!(
            scope_for_authority_row(
                &missing_network,
                "unknown",
                &scopes,
                "network-1",
                "member_did",
            )
            .unwrap(),
            Some(("network-1".into(), "did:key:member-a".into()))
        );
        let unattributable = json!({ "request_id": "unknown" });
        assert!(scope_for_authority_row(
            &unattributable,
            "unknown",
            &scopes,
            "network-1",
            "member_did",
        )
        .is_err());
        let global = EnrollmentProjection::conflicted(Some("network-1".into()), "root invalid");
        assert!(global.conflict.is_some());
        assert!(global.active.is_empty());
        assert!(global.pending.is_empty());
    }

    #[test]
    fn malformed_candidate_requests_are_quarantined_at_the_narrowest_scope() {
        assert_eq!(
            attribute_candidate_request(
                &json!({
                    "network_id": "network-1",
                    "request_id": "request-a",
                    "candidate_did": "did:key:member-a"
                }),
                "network-1",
            ),
            CandidateRequestAttribution::Exact {
                request_id: "request-a".into(),
                scope: ("network-1".into(), "did:key:member-a".into()),
            }
        );
        assert_eq!(
            attribute_candidate_request(
                &json!({"network_id": "network-1", "request_id": "request-a"}),
                "network-1",
            ),
            CandidateRequestAttribution::RequestOnly("request-a".into())
        );
        assert_eq!(
            attribute_candidate_request(
                &json!({"network_id": "network-1", "candidate_did": "did:key:member-a"}),
                "network-1",
            ),
            CandidateRequestAttribution::ScopeOnly(
                ("network-1".into(), "did:key:member-a".into(),)
            )
        );
        assert_eq!(
            attribute_candidate_request(&json!({"candidate_did": "did:key:member-a"}), "network-1",),
            CandidateRequestAttribution::ScopeOnly(
                ("network-1".into(), "did:key:member-a".into(),)
            )
        );
        assert_eq!(
            attribute_candidate_request(&json!({"request_id": "unattributable"}), "network-1"),
            CandidateRequestAttribution::RequestOnly("unattributable".into())
        );
        for row in [
            json!({"network_id": "network-2", "request_id": "foreign"}),
            json!({"network_id": "network-1"}),
        ] {
            assert_eq!(
                attribute_candidate_request(&row, "network-1"),
                CandidateRequestAttribution::ForeignOrUnattributable
            );
        }
    }
}
