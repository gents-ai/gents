//! Renewal notices for members of this node's network.
//!
//! A lease cannot be extended in place: the signed decision and its active
//! revision carry one fixed expiry, bound to a request the member signed inside
//! a short offer window. Renewal is therefore a superseding re-enrollment of
//! the same member, which every protocol-2 node accepts. The owner cannot sign
//! the member's request, so it reports the generations that need one.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use gents_protocol::enrollment::{AuthorizationLease, AuthorizationLeaseState};

use super::enrollment_store::{ActiveEnrollment, EnrollmentProjection};

/// One notice per signed generation entering the renewal window, and one more
/// when it expires, so a sweep loop does not repeat itself every tick.
#[derive(Debug, Default)]
pub(super) struct RenewalNotices {
    noticed: BTreeSet<(String, AuthorizationLeaseState)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenewalNotice {
    pub(super) request_id: String,
    pub(super) member_did: String,
    pub(super) member_peer: String,
    pub(super) authorization_sequence: u64,
    pub(super) authorization_expires_at: String,
    pub(super) lease: AuthorizationLease,
}

impl RenewalNotices {
    /// The generations whose lease state changed into one that needs renewal
    /// since the last call. Superseded or revoked generations are forgotten.
    pub(super) fn observe(
        &mut self,
        projection: &EnrollmentProjection,
        now: DateTime<Utc>,
    ) -> Vec<RenewalNotice> {
        let current = projection
            .active
            .iter()
            .map(|active| active.revision.revision_id.clone())
            .collect::<BTreeSet<_>>();
        self.noticed
            .retain(|(revision_id, _)| current.contains(revision_id));
        projection
            .active
            .iter()
            .filter_map(|active| self.notice(active, now))
            .collect()
    }

    fn notice(&mut self, active: &ActiveEnrollment, now: DateTime<Utc>) -> Option<RenewalNotice> {
        let lease = AuthorizationLease::at(&active.revision.authorization_expires_at, now);
        if !lease.needs_renewal()
            || !self
                .noticed
                .insert((active.revision.revision_id.clone(), lease.state))
        {
            return None;
        }
        Some(RenewalNotice {
            request_id: active.request.request_id.clone(),
            member_did: active.request.candidate_did.clone(),
            member_peer: active.request.candidate_peer.clone(),
            authorization_sequence: active.revision.sequence,
            authorization_expires_at: active.revision.authorization_expires_at.clone(),
            lease,
        })
    }
}

pub(super) fn log_renewal_notice(notice: &RenewalNotice) {
    tracing::warn!(
        request_id = %notice.request_id,
        member_did = %notice.member_did,
        member_peer = %notice.member_peer,
        authorization_sequence = notice.authorization_sequence,
        authorization_expires_at = %notice.authorization_expires_at,
        lease_state = ?notice.lease.state,
        remaining_days = notice.lease.remaining_days(),
        "enrollment authorization needs renewal: re-enroll the member (fresh offer, its request, approval) before it expires; its routes and remote requests stop at expiry"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::enrollment::{
        AuthorizationRevisionKind, AuthorizationRevisionRecord, EnrollmentDecisionKind,
        EnrollmentDecisionRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
    };

    fn active_enrollment(sequence: u64, expires_at: &str) -> ActiveEnrollment {
        let request = EnrollmentRequestRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            request_id: format!("enroll-{sequence}"),
            request_digest: format!("digest-{sequence}"),
            offer_id: format!("offer-{sequence}"),
            offer_token: format!("token-{sequence}"),
            challenge: format!("challenge-{sequence}"),
            network_id: "network-1".into(),
            admin_did: "did:key:admin".into(),
            server_peer: "server-peer".into(),
            candidate_did: "did:key:member".into(),
            candidate_peer: "member-peer".into(),
            candidate_ticket: "member-ticket".into(),
            owner_agent: "did:key:admin".into(),
            profile: "client".into(),
            client_nonce: format!("nonce-{sequence}"),
            issued_at: "2026-10-30T19:07:00Z".into(),
            expires_at: "2026-10-30T19:12:00Z".into(),
            candidate_sig: vec![1; 64],
        };
        let decision = EnrollmentDecisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            decision_id: format!("decision-{sequence}"),
            request_id: request.request_id.clone(),
            request_digest: request.request_digest.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            candidate_did: request.candidate_did.clone(),
            candidate_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            decision: EnrollmentDecisionKind::Approved,
            agent_ids: None,
            authorization_sequence: sequence,
            authorization_expires_at: expires_at.into(),
            decided_at: "2026-10-30T19:07:47Z".into(),
            signer_did: request.admin_did.clone(),
            admin_sig: vec![2; 64],
        };
        let revision = AuthorizationRevisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            revision_id: format!("authorization-{sequence}"),
            request_id: request.request_id.clone(),
            request_digest: request.request_digest.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            member_did: request.candidate_did.clone(),
            member_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            sequence,
            authorization_expires_at: expires_at.into(),
            kind: AuthorizationRevisionKind::Active,
            issued_at: decision.decided_at.clone(),
            signer_did: request.admin_did.clone(),
            admin_sig: vec![3; 64],
        };
        ActiveEnrollment {
            request_doc_id: format!("request-doc-{sequence}"),
            decision_doc_id: format!("decision-doc-{sequence}"),
            revision_doc_id: format!("revision-doc-{sequence}"),
            route_receipt_doc_id: None,
            request,
            decision,
            revision,
            route_receipt: None,
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    fn projection(active: Vec<ActiveEnrollment>) -> EnrollmentProjection {
        EnrollmentProjection {
            network_id: Some("network-1".into()),
            active,
            ..EnrollmentProjection::default()
        }
    }

    #[test]
    fn a_generation_is_noticed_once_per_state_and_forgotten_when_superseded() {
        let expiring = active_enrollment(1, "2026-11-29T19:07:47Z");
        let mut notices = RenewalNotices::default();

        assert!(notices
            .observe(
                &projection(vec![expiring.clone()]),
                at("2026-11-01T00:00:00Z")
            )
            .is_empty());

        let due = notices.observe(
            &projection(vec![expiring.clone()]),
            at("2026-11-25T00:00:00Z"),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].lease.state, AuthorizationLeaseState::RenewalDue);
        assert_eq!(due[0].lease.remaining_days(), 4);
        assert_eq!(due[0].authorization_sequence, 1);
        assert!(notices
            .observe(
                &projection(vec![expiring.clone()]),
                at("2026-11-26T00:00:00Z")
            )
            .is_empty());

        let expired = notices.observe(
            &projection(vec![expiring.clone()]),
            at("2026-11-30T00:00:00Z"),
        );
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].lease.state, AuthorizationLeaseState::Expired);
        assert!(notices
            .observe(&projection(vec![expiring]), at("2026-11-30T00:01:00Z"))
            .is_empty());

        let renewed = active_enrollment(2, "2026-12-29T19:07:47Z");
        assert!(notices
            .observe(
                &projection(vec![renewed.clone()]),
                at("2026-11-30T00:02:00Z")
            )
            .is_empty());
        assert!(notices.noticed.is_empty());
        let due = notices.observe(&projection(vec![renewed]), at("2026-12-23T00:00:00Z"));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].authorization_sequence, 2);
    }
}
