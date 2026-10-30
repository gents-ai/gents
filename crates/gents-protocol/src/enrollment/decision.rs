use serde::{Deserialize, Serialize};

use super::canonical::{canonical_domain_payload, derive_enrollment_id, parse_canonical_timestamp};
use super::error::{EnrollmentError, Result};
use super::request::EnrollmentRequestRecord;
use super::{
    DECISION_SIGNATURE_DOMAIN, ENROLLMENT_PROTOCOL_VERSION,
    MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentDecisionKind {
    Approved,
    Denied,
}

/// Why the runtime refused an operator's enrollment decision, as the
/// decision endpoint reports it beside the error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentDecisionRefusalCode {
    /// The request is not (yet) a fresh pending request in this projection.
    NotPending,
    /// Two requests used one offer's challenge; both are refused.
    ChallengeReused,
    /// The request conflicts with other enrollment rows.
    Conflicted,
}

impl EnrollmentDecisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentDecisionRecord {
    pub protocol_version: u8,
    pub decision_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub owner_agent: String,
    pub decision: EnrollmentDecisionKind,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
    pub decided_at: String,
    pub signer_did: String,
    pub admin_sig: Vec<u8>,
}

impl EnrollmentDecisionRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        let sequence = self.authorization_sequence.to_string();
        canonical_domain_payload(
            DECISION_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.decision_id,
                &self.request_id,
                &self.request_digest,
                &self.network_id,
                &self.admin_did,
                &self.candidate_did,
                &self.candidate_peer,
                &self.owner_agent,
                self.decision.as_str(),
                sequence.as_str(),
                &self.authorization_expires_at,
                &self.decided_at,
                &self.signer_did,
            ],
        )
    }

    pub fn validate_against_request(&self, request: &EnrollmentRequestRecord) -> Result<()> {
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedDecisionVersion {
                version: self.protocol_version,
            });
        }
        if self.request_id != request.request_id {
            return Err(EnrollmentError::DecisionRequestIdMismatch);
        }
        if self.request_digest != request.request_digest {
            return Err(EnrollmentError::DecisionRequestDigestMismatch);
        }
        if self.network_id != request.network_id {
            return Err(EnrollmentError::DecisionNetworkIdMismatch);
        }
        if self.admin_did != request.admin_did {
            return Err(EnrollmentError::DecisionAdminDidMismatch);
        }
        if self.candidate_did != request.candidate_did {
            return Err(EnrollmentError::DecisionCandidateDidMismatch);
        }
        if self.candidate_peer != request.candidate_peer {
            return Err(EnrollmentError::DecisionCandidatePeerMismatch);
        }
        if self.owner_agent != request.owner_agent {
            return Err(EnrollmentError::DecisionOwnerAgentMismatch);
        }
        if self.signer_did != request.admin_did {
            return Err(EnrollmentError::DecisionSignerMismatch);
        }
        if self.decision_id != derive_decision_id(&request.request_id, &request.request_digest) {
            return Err(EnrollmentError::DecisionIdMismatch);
        }
        match self.decision {
            EnrollmentDecisionKind::Approved => {
                if self.authorization_sequence == 0 {
                    return Err(EnrollmentError::ApprovedWithoutSequence);
                }
                let expires = parse_canonical_timestamp(
                    "decision authorization_expires_at",
                    &self.authorization_expires_at,
                )?;
                let decided = parse_canonical_timestamp("decision decided_at", &self.decided_at)?;
                if expires <= decided {
                    return Err(EnrollmentError::ApprovalLeaseNotAfterDecision);
                }
                if expires - decided
                    > chrono::Duration::seconds(MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS as i64)
                {
                    return Err(EnrollmentError::ApprovalLeaseTooLong);
                }
            }
            EnrollmentDecisionKind::Denied => {
                if self.authorization_sequence != 0 {
                    return Err(EnrollmentError::DeniedWithSequence);
                }
                if self.authorization_expires_at != self.decided_at {
                    return Err(EnrollmentError::DeniedLeaseNotDecision);
                }
                parse_canonical_timestamp(
                    "denied decision authorization_expires_at",
                    &self.authorization_expires_at,
                )?;
            }
        }
        if self.authorization_sequence > i64::MAX as u64 {
            return Err(EnrollmentError::DecisionSequenceOverflow);
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::DecisionSignatureLength);
        }
        let request_issued = parse_canonical_timestamp("request issued_at", &request.issued_at)?;
        let request_expires = parse_canonical_timestamp("request expires_at", &request.expires_at)?;
        let decided = parse_canonical_timestamp("decision decided_at", &self.decided_at)?;
        if !(request_issued <= decided && decided <= request_expires) {
            return Err(EnrollmentError::DecisionOutsideRequestWindow);
        }
        Ok(())
    }
}

pub fn derive_decision_id(request_id: &str, request_digest: &str) -> String {
    format!(
        "decision-{}",
        derive_enrollment_id(
            "gents-enrollment-decision-id-v1",
            &[request_id, request_digest],
        )
    )
}
