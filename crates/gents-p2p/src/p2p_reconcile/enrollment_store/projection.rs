use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::enrollment::{
    AuthorizationRevisionRecord, EnrollmentDecisionRecord, EnrollmentRequestRecord,
    EnrollmentRouteReceiptRecord,
};

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
    pub revoked: Vec<RevokedEnrollment>,
    pub next_authorization_sequences: BTreeMap<(String, String), u64>,
    pub scoped_conflicts: BTreeMap<(String, String), String>,
    pub request_conflicts: BTreeMap<String, String>,
    pub reused_challenge_requests: BTreeSet<String>,
    pub conflict: Option<String>,
}

/// An operator decision the projection refuses before writing it.
#[derive(Debug, thiserror::Error)]
pub enum EnrollmentDecisionRefusal {
    #[error("enrollment request {request_id} is conflicted: {reason}")]
    Conflicted {
        request_id: String,
        reason: String,
        reused_challenge: bool,
    },
    #[error("no fresh pending enrollment request {request_id}")]
    NotPending { request_id: String },
}

impl EnrollmentDecisionRefusal {
    pub fn in_chain<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        gents_protocol::error_chain::find_source::<Self>(error)
    }

    pub fn code(&self) -> gents_protocol::enrollment::EnrollmentDecisionRefusalCode {
        use gents_protocol::enrollment::EnrollmentDecisionRefusalCode as Code;
        match self {
            Self::Conflicted {
                reused_challenge: true,
                ..
            } => Code::ChallengeReused,
            Self::Conflicted { .. } => Code::Conflicted,
            Self::NotPending { .. } => Code::NotPending,
        }
    }
}

impl EnrollmentProjection {
    pub(in crate::p2p_reconcile) fn current_for_renewal_intent(
        &self,
        intent: &gents_protocol::enrollment::RenewalIntent,
        now: DateTime<Utc>,
    ) -> Result<&ActiveEnrollment> {
        use gents_protocol::enrollment::{AuthorizationLease, AuthorizationLeaseState};
        intent.validate_at(now)?;
        anyhow::ensure!(
            self.conflict.is_none(),
            "enrollment authority is conflicted"
        );
        let scope = (intent.network_id.clone(), intent.member_did.clone());
        anyhow::ensure!(
            !self.scoped_conflicts.contains_key(&scope),
            "member authority is conflicted"
        );
        let active = self
            .active
            .iter()
            .find(|active| {
                active.request.network_id == intent.network_id
                    && active.request.candidate_did == intent.member_did
            })
            .context("member has no active enrollment")?;
        anyhow::ensure!(
            active.request.admin_did == intent.admin_did
                && active.request.server_peer == intent.server_peer
                && active.request.candidate_peer == intent.member_peer
                && active.revision.sequence == intent.predecessor_sequence,
            "renewal intent changes the current enrollment"
        );
        let next = intent
            .predecessor_sequence
            .checked_add(1)
            .context("authorization sequence exhausted")?;
        anyhow::ensure!(
            self.next_authorization_sequences.get(&scope) == Some(&next),
            "enrollment generation changed"
        );
        anyhow::ensure!(
            AuthorizationLease::at(&active.revision.authorization_expires_at, now).state
                == AuthorizationLeaseState::RenewalDue,
            "enrollment is not within its live renewal window"
        );
        Ok(active)
    }

    pub(super) fn conflicted(network_id: Option<String>, error: impl std::fmt::Display) -> Self {
        Self {
            network_id,
            conflict: Some(error.to_string()),
            ..Self::default()
        }
    }

    pub fn pending_for_decision(
        &self,
        request_id: &str,
    ) -> Result<&PendingEnrollment, EnrollmentDecisionRefusal> {
        if let Some(reason) = self.request_conflicts.get(request_id) {
            return Err(EnrollmentDecisionRefusal::Conflicted {
                request_id: request_id.to_string(),
                reason: reason.clone(),
                reused_challenge: self.reused_challenge_requests.contains(request_id),
            });
        }
        self.pending
            .iter()
            .find(|pending| pending.request.request_id == request_id)
            .ok_or_else(|| EnrollmentDecisionRefusal::NotPending {
                request_id: request_id.to_string(),
            })
    }

    /// Evaluate a pending renewal against the current verified projection.
    /// The approval owner must hold its decision lock, authenticate the live
    /// candidate and read the operator policy under the same lock before use.
    pub fn renewal_lease_for(
        &self,
        request_id: &str,
        enabled: bool,
        configured_limit: Option<u64>,
        now: DateTime<Utc>,
    ) -> Result<Duration> {
        use gents_protocol::enrollment::{
            renewal_challenge_matches, AuthorizationLease, AuthorizationLeaseState,
            MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
        };

        anyhow::ensure!(enabled, "automatic enrollment renewal is disabled");
        anyhow::ensure!(
            self.conflict.is_none(),
            "enrollment authority is conflicted"
        );
        let pending = self.pending_for_decision(request_id)?;
        let request = &pending.request;
        let scope = (request.network_id.clone(), request.candidate_did.clone());
        anyhow::ensure!(
            !self.scoped_conflicts.contains_key(&scope),
            "enrollment member authority is conflicted"
        );
        let request_issued = DateTime::parse_from_rfc3339(&request.issued_at)?.with_timezone(&Utc);
        let request_expires =
            DateTime::parse_from_rfc3339(&request.expires_at)?.with_timezone(&Utc);
        anyhow::ensure!(
            request_issued <= now && now < request_expires,
            "renewal request is not currently fresh"
        );
        let active = self
            .active
            .iter()
            .find(|entry| {
                entry.request.network_id == request.network_id
                    && entry.request.candidate_did == request.candidate_did
            })
            .context("no active enrollment for renewal")?;
        let next_sequence = active
            .revision
            .sequence
            .checked_add(1)
            .context("enrollment authorization sequence exhausted")?;
        anyhow::ensure!(
            self.next_authorization_sequences.get(&scope) == Some(&next_sequence),
            "enrollment authorization generation changed or is incomplete"
        );
        anyhow::ensure!(
            active.request.candidate_peer == request.candidate_peer
                && active.request.owner_node_did == request.owner_node_did
                && active.request.profile == request.profile
                && active.request.admin_did == request.admin_did
                && active.request.server_peer == request.server_peer,
            "renewal changes the current enrollment scope"
        );
        anyhow::ensure!(
            AuthorizationLease::at(&active.revision.authorization_expires_at, now).state
                == AuthorizationLeaseState::RenewalDue,
            "enrollment is not within its live renewal window"
        );
        anyhow::ensure!(
            renewal_challenge_matches(
                &request.challenge,
                &request.network_id,
                &request.candidate_did,
                &request.candidate_peer,
                active.revision.sequence,
            ),
            "renewal offer does not bind the current enrollment generation"
        );
        let issued = DateTime::parse_from_rfc3339(&active.decision.decided_at)?;
        let expires = DateTime::parse_from_rfc3339(&active.revision.authorization_expires_at)?;
        let original = u64::try_from((expires - issued).num_seconds())
            .context("current enrollment lease is invalid")?;
        anyhow::ensure!(
            (1..=MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS).contains(&original),
            "current enrollment lease is outside the protocol limit"
        );
        let lease = configured_limit.unwrap_or(original);
        anyhow::ensure!(
            (1..=MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS).contains(&lease),
            "renewal lease is outside the protocol limit"
        );
        Ok(Duration::from_secs(lease))
    }
}
