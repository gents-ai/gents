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

pub(super) fn log_renewal_notice(notice: &RenewalNotice, automatic_enabled: bool) {
    if automatic_enabled && notice.lease.state == AuthorizationLeaseState::RenewalDue {
        tracing::warn!(
            request_id = %notice.request_id,
            member_did = %notice.member_did,
            authorization_sequence = notice.authorization_sequence,
            authorization_expires_at = %notice.authorization_expires_at,
            "enrollment authorization is nearing expiry; automatic renewal is enabled, but peers without renewal support still require re-enrollment"
        );
        return;
    }
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
    use super::super::enrollment_store::PendingEnrollment;
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
            candidate_name: None,
            candidate_ticket: "member-ticket".into(),
            owner_node_did: "did:key:admin".into(),
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
            owner_node_did: request.owner_node_did.clone(),
            decision: EnrollmentDecisionKind::Approved,
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
            owner_node_did: request.owner_node_did.clone(),
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
    fn generated_intent_cases_match_current_generation_projection() {
        use chrono::{SecondsFormat, TimeZone};
        let timestamp = |seconds: i64| {
            Utc.timestamp_opt(1_800_000_000 + seconds, 0)
                .single()
                .unwrap()
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        };
        gents_lean_vocab::families::enrollment_auto_renewal_intent_projection::replay(|case| {
            let current = &case.current;
            let input = &case.intent;
            let model_now = case.now as i64;
            let now_seconds = model_now * 30;
            let now = Utc
                .timestamp_opt(1_800_000_000 + now_seconds, 0)
                .single()
                .unwrap();
            let expires = now_seconds + (current.expires_at as i64 - model_now) * 60_480;
            let mut active = active_enrollment(current.sequence, &timestamp(expires));
            active.request.network_id = current.network.clone();
            active.request.candidate_did = current.member.clone();
            active.request.candidate_peer = current.peer.clone();
            let mut state = projection(if current.revoked {
                Vec::new()
            } else {
                vec![active.clone()]
            });
            if current.conflict {
                state.conflict = Some("conflict".into());
            }
            state.next_authorization_sequences.insert(
                (
                    active.request.network_id.clone(),
                    active.request.candidate_did.clone(),
                ),
                current.next_sequence,
            );
            let intent = gents_protocol::enrollment::RenewalIntent {
                network_id: input.network.clone(),
                admin_did: active.request.admin_did.clone(),
                server_peer: active.request.server_peer.clone(),
                member_did: input.member.clone(),
                member_peer: input.peer.clone(),
                predecessor_sequence: input.predecessor_sequence,
                issued_at: timestamp(input.issued_at as i64 * 30),
                expires_at: timestamp(input.expires_at as i64 * 30),
                nonce: "generated".into(),
                member_sig: vec![1; 64],
            };
            gents_lean_vocab::families::enrollment_auto_renewal_intent_projection::IntentProjection {
                projection_allowed: state.current_for_renewal_intent(&intent, now).is_ok(),
            }
        });
    }

    #[test]
    fn generated_approval_cases_match_renewal_projection() {
        use chrono::{SecondsFormat, TimeZone};
        let timestamp = |seconds: i64| {
            Utc.timestamp_opt(1_800_000_000 + seconds, 0)
                .single()
                .unwrap()
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        };
        gents_lean_vocab::families::enrollment_auto_renewal_approval::replay(|case| {
            let current = &case.current;
            let proposed = &case.proposal;
            let limit = current.renewal_limit;
            let model_now = case.now as i64;
            let now_seconds = model_now * 30;
            let now = Utc
                .timestamp_opt(1_800_000_000 + now_seconds, 0)
                .single()
                .unwrap();
            let expires = now_seconds + (current.expires_at as i64 - model_now) * 60_480;
            let mut active = active_enrollment(current.sequence, &timestamp(expires));
            active.request.network_id = current.network.clone();
            active.request.candidate_did = current.member.clone();
            active.request.candidate_peer = current.peer.clone();
            active.request.owner_node_did = current.owner.clone();
            active.request.profile = current.profile.clone();
            active.decision.decided_at = timestamp(expires - current.lease_seconds as i64 * 86_400);
            let mut request = active.request.clone();
            request.request_id = "renewal".into();
            request.network_id = proposed.network.clone();
            request.candidate_did = proposed.member.clone();
            request.candidate_peer = proposed.peer.clone();
            request.owner_node_did = proposed.owner.clone();
            request.profile = proposed.profile.clone();
            request.issued_at = timestamp(now_seconds - 60);
            request.expires_at = timestamp(now_seconds + 60);
            request.challenge = gents_protocol::enrollment::renewal_challenge(
                &request.network_id,
                &request.candidate_did,
                &request.candidate_peer,
                proposed.predecessor_sequence,
                "0123456789abcdef0123456789abcdef",
            )
            .unwrap();
            let mut state = projection(if current.revoked {
                Vec::new()
            } else {
                vec![active.clone()]
            });
            if current.authority_conflict {
                state.conflict = Some("conflict".into());
            }
            state.next_authorization_sequences.insert(
                (
                    active.request.network_id.clone(),
                    active.request.candidate_did.clone(),
                ),
                current.next_sequence,
            );
            state.pending.push(PendingEnrollment {
                request_doc_id: "generated".into(),
                offer_token: request.offer_token.clone(),
                request,
            });
            let actual =
                state.renewal_lease_for("renewal", current.enabled, limit.map(|n| n * 86_400), now);
            gents_lean_vocab::families::enrollment_auto_renewal_approval::Approval {
                projection_allowed: actual.is_ok(),
                lease: actual.ok().map(|lease| {
                    assert_eq!(lease.as_secs() % 86_400, 0, "{}", case.name);
                    lease.as_secs() / 86_400
                }),
            }
        });
    }

    #[test]
    fn intent_is_limited_to_the_current_live_generation() {
        let mut state = projection(vec![active_enrollment(1, "2026-11-29T19:07:47Z")]);
        state
            .next_authorization_sequences
            .insert(("network-1".into(), "did:key:member".into()), 2);
        let mut intent = gents_protocol::enrollment::RenewalIntent {
            network_id: "network-1".into(),
            admin_did: "did:key:admin".into(),
            server_peer: "server-peer".into(),
            member_did: "did:key:member".into(),
            member_peer: "member-peer".into(),
            predecessor_sequence: 1,
            issued_at: "2026-11-25T00:00:00Z".into(),
            expires_at: "2026-11-25T00:05:00Z".into(),
            nonce: "nonce".into(),
            member_sig: vec![1; 64],
        };
        let now = at("2026-11-25T00:01:00Z");
        assert!(state.current_for_renewal_intent(&intent, now).is_ok());
        intent.member_peer = "other".into();
        assert!(state.current_for_renewal_intent(&intent, now).is_err());
        intent.member_peer = "member-peer".into();
        assert!(state
            .current_for_renewal_intent(&intent, at("2026-11-25T00:05:00Z"))
            .is_err());
        state
            .next_authorization_sequences
            .insert(("network-1".into(), "did:key:member".into()), 3);
        assert!(state.current_for_renewal_intent(&intent, now).is_err());
    }

    #[test]
    fn renewal_policy_refuses_scope_changes_expiry_and_superseded_offers() {
        let current = active_enrollment(1, "2026-11-29T19:07:47Z");
        let mut request = current.request.clone();
        request.request_id = "renewal".into();
        request.issued_at = "2026-11-01T00:00:00Z".into();
        request.expires_at = "2026-12-01T00:00:00Z".into();
        request.challenge = gents_protocol::enrollment::renewal_challenge(
            &request.network_id,
            &request.candidate_did,
            &request.candidate_peer,
            current.revision.sequence,
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let mut state = projection(vec![current]);
        state
            .next_authorization_sequences
            .insert(("network-1".into(), "did:key:member".into()), 2);
        state.pending.push(PendingEnrollment {
            request_doc_id: "pending".into(),
            offer_token: request.offer_token.clone(),
            request,
        });
        let now = at("2026-11-25T00:00:00Z");
        assert_eq!(
            state
                .renewal_lease_for("renewal", true, None, now)
                .unwrap()
                .as_secs(),
            30 * 86400
        );
        assert_eq!(
            state
                .renewal_lease_for("renewal", true, Some(60 * 86400), now)
                .unwrap()
                .as_secs(),
            60 * 86400
        );
        assert!(state
            .renewal_lease_for("renewal", false, None, now)
            .is_err());
        assert!(state
            .renewal_lease_for("renewal", true, Some(91 * 86400), now)
            .is_err());
        assert!(state
            .renewal_lease_for("renewal", true, None, at("2026-11-29T19:07:47Z"))
            .is_err());
        assert!(state
            .renewal_lease_for("renewal", true, None, at("2026-12-01T00:00:00Z"))
            .is_err());
        assert!(state
            .renewal_lease_for("renewal", true, None, at("2026-11-01T00:00:00Z"))
            .is_err());
        state.pending[0].request.candidate_peer = "other-peer".into();
        assert!(state.renewal_lease_for("renewal", true, None, now).is_err());
        state.pending[0].request.candidate_peer = "member-peer".into();
        state.scoped_conflicts.insert(
            ("network-1".into(), "did:key:member".into()),
            "conflict".into(),
        );
        assert!(state.renewal_lease_for("renewal", true, None, now).is_err());
        state.scoped_conflicts.clear();
        state
            .next_authorization_sequences
            .insert(("network-1".into(), "did:key:member".into()), 3);
        assert!(state.renewal_lease_for("renewal", true, None, now).is_err());
        state.active[0].revision.sequence = 2;
        assert!(state.renewal_lease_for("renewal", true, None, now).is_err());
        state.active.clear();
        assert!(state.renewal_lease_for("renewal", true, None, now).is_err());
    }

    #[test]
    fn renewal_notices_match_the_lean_model() {
        let now = at("2026-11-25T00:00:00Z");
        gents_lean_vocab::families::enrollment_renewal_notices_trace::replay::<String>(|trace| {
            let mut notices = RenewalNotices::default();
            trace
                .steps
                .iter()
                .map(|step| {
                    let expires = match step.state.as_str() {
                        "invalid" => "soon".to_string(),
                        state => {
                            let days = match state {
                                "current" => 30,
                                "renewal_due" => 4,
                                "expired" => -1,
                                other => panic!("unknown lease state {other}"),
                            };
                            (now + chrono::Duration::days(days))
                                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                        }
                    };
                    let active = if step.active {
                        vec![active_enrollment(step.sequence, &expires)]
                    } else {
                        Vec::new()
                    };
                    let mut found = notices.observe(&projection(active), now);
                    assert!(found.len() <= 1, "{found:?}");
                    found.pop().map(|notice| {
                        serde_json::to_value(notice.lease.state)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_string()
                    })
                })
                .collect()
        });
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
