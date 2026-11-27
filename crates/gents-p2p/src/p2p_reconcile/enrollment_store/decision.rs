use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_p2p_adapter::TransportPeerId;
use gents_protocol::enrollment::{
    decode_offer, derive_decision_id, derive_revision_id,
    AuthorizationRevisionKind as WireRevisionKind, AuthorizationRevisionRecord,
    EnrollmentDecisionKind as WireDecisionKind, EnrollmentDecisionRecord, EnrollmentRequestRecord,
    DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS, ENROLLMENT_PROTOCOL_VERSION,
};
use tokio::time::timeout;

use super::graphql::{decision_mutation, revision_mutation};
use super::GraphqlEnrollmentStore;

/// What a decision withdraws: a denied request's own grant, or every grant of
/// a revoked membership (Lean `Identity.EnrollmentGrant.applyDecision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WithdrawalScope {
    Request,
    Membership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentDecisionOutcome {
    pub request_id: String,
    pub state: &'static str,
    pub decision_doc_id: String,
    pub revision_doc_id: Option<String>,
    pub delivery_pending: bool,
    /// The member's Node administration removal waits for a client of the
    /// Node owner or a Node administrator (Lean
    /// `Identity.EnrollmentGrant.oweRevoke`).
    pub administration_removal_pending: bool,
}

impl GraphqlEnrollmentStore {
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
        self.decide_request_inner(request_id, kind, Some(lease))
            .await
    }

    pub async fn decide_automatic_renewal(
        &self,
        request_id: &str,
    ) -> Result<EnrollmentDecisionOutcome> {
        self.decide_request_inner(request_id, WireDecisionKind::Approved, None)
            .await
    }

    async fn decide_request_inner(
        &self,
        request_id: &str,
        kind: WireDecisionKind,
        operator_lease: Option<Duration>,
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
                administration_removal_pending: false,
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
            let administration_removal_pending = self
                .finish_grant_withdrawal(
                    request_id,
                    &request.candidate_did,
                    WithdrawalScope::Request,
                )
                .await?;
            let delivery_pending = self
                .deliver_terminal(&request, &decision_doc_id, None, None)
                .await;
            return Ok(EnrollmentDecisionOutcome {
                request_id: request_id.to_string(),
                state: "denied",
                decision_doc_id,
                revision_doc_id: None,
                delivery_pending,
                administration_removal_pending,
            });
        }
        let pending = projection.pending_for_decision(request_id)?;

        if kind == WireDecisionKind::Approved {
            let offer = decode_offer(&pending.request.offer_token)?;
            super::super::enrollment_candidate::verify_offer_live_schema(&self.node, &offer)
                .await
                .context("checking invitation schema before approval")?;
        }

        let mut decided_at = self.verify_live_candidate(&pending.request).await?;
        let lease = match operator_lease {
            Some(lease) => lease,
            None => {
                let policy = self.renewal_policy(&pending.request.network_id).await?;
                let latest = self.load_projection().await?;
                anyhow::ensure!(
                    latest.pending_for_decision(request_id)?.request == pending.request,
                    "renewal request changed during authorization"
                );
                let scope = (
                    pending.request.network_id.clone(),
                    pending.request.candidate_did.clone(),
                );
                anyhow::ensure!(
                    latest.next_authorization_sequences.get(&scope)
                        == projection.next_authorization_sequences.get(&scope),
                    "enrollment generation changed during authorization"
                );
                decided_at = Utc::now();
                latest.renewal_lease_for(
                    request_id,
                    policy.enabled,
                    policy.lease_seconds,
                    decided_at,
                )?
            }
        };
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
            owner_node_did: pending.request.owner_node_did.clone(),
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
                owner_node_did: pending.request.owner_node_did.clone(),
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

        let mutation = decision_mutation(
            &decision,
            revision.as_ref(),
            &super::super::publisher_field(&self.node)?,
        );
        let committed = async {
            let mut collections = vec!["NetworkEnrollmentDecision"];
            if revision.is_some() {
                collections.push("NetworkAuthorizationRevision");
            }
            let response = self
                .publish_records(
                    "p2p.write_enrollment_decision",
                    &mutation,
                    &collections,
                    &pending.request.candidate_did,
                    None,
                )
                .await?;
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
        /* Lean `Identity.EnrollmentGrant.applyDecision .deny`: a grant whose
        approval never committed is withdrawn with the denial */
        let administration_removal_pending = match kind {
            WireDecisionKind::Denied => {
                self.finish_grant_withdrawal(
                    request_id,
                    &pending.request.candidate_did,
                    WithdrawalScope::Request,
                )
                .await?
            }
            WireDecisionKind::Approved => false,
        };
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
            administration_removal_pending,
        })
    }

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
            let administration_removal_pending = self
                .finish_grant_withdrawal(
                    request_id,
                    &revoked.request.candidate_did,
                    WithdrawalScope::Membership,
                )
                .await?;
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
                administration_removal_pending,
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
            owner_node_did: active.request.owner_node_did.clone(),
            sequence,
            authorization_expires_at: active.decision.authorization_expires_at.clone(),
            kind,
            issued_at,
            signer_did: self.identity.did().to_string(),
            admin_sig: Vec::new(),
        };
        revision.admin_sig = self.identity.sign(&revision.signing_payload()).await?;
        revision.validate_against_approval(&active.request, &active.decision)?;
        let withdrawal = self
            .prepare_grant_withdrawal(
                request_id,
                &active.request.candidate_did,
                WithdrawalScope::Membership,
            )
            .await?;
        let administration_removal_pending = withdrawal
            .as_ref()
            .is_some_and(|withdrawal| !withdrawal.owing_doc_ids.is_empty());
        let mutation = revision_mutation(&revision, &super::super::publisher_field(&self.node)?);
        let response = self
            .publish_records(
                "p2p.write_enrollment_revocation",
                &mutation,
                &["NetworkAuthorizationRevision"],
                &active.request.candidate_did,
                withdrawal.as_ref(),
            )
            .await;
        let revision_doc_id = match response {
            Ok(response) => gents_protocol::graphql::extract_mutation_doc_id(
                &response,
                "NetworkAuthorizationRevision",
            )?,
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
            administration_removal_pending,
        })
    }

    /// Lean `Identity.EnrollmentGrant.oweRevoke` for a denied request, or
    /// `oweRevokeMember` for a revoked membership, whose every generation's
    /// receipt is withdrawn: plan the withdrawal and revoke its node
    /// permissions now, as the NAC owner, before the revision commits; the
    /// receipts retire in the revision's transaction, owing the Node
    /// `administrator` relation's withdrawal to the owner's client.
    async fn prepare_grant_withdrawal(
        &self,
        request_id: &str,
        member: &str,
        scope: WithdrawalScope,
    ) -> Result<Option<gents_store::identity::EnrollmentGrantWithdrawal>> {
        let runtime = self.runtime_access()?;
        let receipts = gents_store::identity::load_enrollment_grant_receipts(&runtime, member)
            .await
            .context("reading the enrollment grant receipts before revocation")?;
        let withdrawal = match scope {
            WithdrawalScope::Request => {
                gents_store::identity::EnrollmentGrantWithdrawal::plan(request_id, &receipts)?
            }
            WithdrawalScope::Membership => {
                gents_store::identity::EnrollmentGrantWithdrawal::plan_membership(
                    member, &receipts,
                )?
            }
        };
        if let Some(withdrawal) = &withdrawal {
            withdrawal
                .revoke_node_permissions(&runtime)
                .await
                .context("revoking the node permissions an enrollment grant derived")?;
        }
        Ok(withdrawal)
    }

    /// A revision that committed before its grant's withdrawal finished (or
    /// before receipts existed) leaves a live receipt; retire it now.
    async fn finish_grant_withdrawal(
        &self,
        request_id: &str,
        member: &str,
        scope: WithdrawalScope,
    ) -> Result<bool> {
        let Some(withdrawal) = self
            .prepare_grant_withdrawal(request_id, member, scope)
            .await?
        else {
            return Ok(false);
        };
        self.runtime_access()?
            .transact_write("p2p.withdraw_enrollment_grant", |txn| {
                let withdrawal = &withdrawal;
                Box::pin(async move {
                    withdrawal.retire_owing_in_txn(txn).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        Ok(!withdrawal.owing_doc_ids.is_empty())
    }

    /// Lean `Identity.EnrollmentGrant.membershipLapsed`: withdraw, as a
    /// revocation does (`Step.expire`), the grant of every member all of
    /// whose current authorization leases ended on the lapse clock, decided
    /// on a projection read under the decision lock so a renewal of the same
    /// member is decided before or after it, never interleaved.
    pub(in crate::p2p_reconcile) async fn withdraw_lapsed_memberships(
        &self,
        wall_clock: impl FnOnce() -> i64,
    ) -> Result<()> {
        let _guard = self.decision_lock.lock().await;
        let projection = self.load_projection().await?;
        let provider = gents_store::local::document_acp::node_provider_time(&self.node);
        let wall = u64::try_from(wall_clock()).unwrap_or(0);
        let mut members: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
        for active in &projection.active {
            let expires = DateTime::parse_from_rfc3339(&active.revision.authorization_expires_at)
                .context("parsing an enrollment authorization lease")?;
            members
                .entry(active.request.candidate_did.as_str())
                .or_default()
                .push(u64::try_from(expires.timestamp()).unwrap_or(0));
        }
        for (member, leases) in members {
            if gents_store::identity::membership_lapsed(provider, wall, &leases) {
                self.finish_grant_withdrawal("", member, WithdrawalScope::Membership)
                    .await?;
            }
        }
        Ok(())
    }

    async fn verify_live_candidate(
        &self,
        request: &EnrollmentRequestRecord,
    ) -> Result<DateTime<Utc>> {
        let p2p =
            crate::node_access::operations(&self.node).context("runtime P2P is unavailable")?;
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
}
