use serde::{Deserialize, Serialize};

use super::canonical::{canonical_domain_payload, derive_enrollment_id, parse_canonical_timestamp};
use super::decision::{EnrollmentDecisionKind, EnrollmentDecisionRecord};
use super::error::{EnrollmentError, Result};
use super::request::EnrollmentRequestRecord;
use super::{ENROLLMENT_PROTOCOL_VERSION, REVISION_SIGNATURE_DOMAIN};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationRevisionKind {
    Active,
    Revoked,
}

impl AuthorizationRevisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationRevisionRecord {
    pub protocol_version: u8,
    pub revision_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub member_did: String,
    pub member_peer: String,
    pub owner_agent: String,
    pub sequence: u64,
    pub authorization_expires_at: String,
    pub kind: AuthorizationRevisionKind,
    pub issued_at: String,
    pub signer_did: String,
    pub admin_sig: Vec<u8>,
}

impl AuthorizationRevisionRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        let sequence = self.sequence.to_string();
        canonical_domain_payload(
            REVISION_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.revision_id,
                &self.request_id,
                &self.request_digest,
                &self.network_id,
                &self.admin_did,
                &self.member_did,
                &self.member_peer,
                &self.owner_agent,
                sequence.as_str(),
                &self.authorization_expires_at,
                self.kind.as_str(),
                &self.issued_at,
                &self.signer_did,
            ],
        )
    }

    pub fn validate_against_approval(
        &self,
        request: &EnrollmentRequestRecord,
        decision: &EnrollmentDecisionRecord,
    ) -> Result<()> {
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedRevisionVersion {
                version: self.protocol_version,
            });
        }
        if decision.decision != EnrollmentDecisionKind::Approved {
            return Err(EnrollmentError::RevisionNotApproved);
        }
        if self.request_id != request.request_id {
            return Err(EnrollmentError::RevisionRequestIdMismatch);
        }
        if self.request_digest != request.request_digest {
            return Err(EnrollmentError::RevisionRequestDigestMismatch);
        }
        if self.network_id != request.network_id {
            return Err(EnrollmentError::RevisionNetworkIdMismatch);
        }
        if self.admin_did != request.admin_did {
            return Err(EnrollmentError::RevisionAdminDidMismatch);
        }
        if self.member_did != request.candidate_did {
            return Err(EnrollmentError::RevisionMemberDidMismatch);
        }
        if self.member_peer != request.candidate_peer {
            return Err(EnrollmentError::RevisionMemberPeerMismatch);
        }
        if self.owner_agent != request.owner_agent {
            return Err(EnrollmentError::RevisionOwnerAgentMismatch);
        }
        if self.signer_did != request.admin_did {
            return Err(EnrollmentError::RevisionSignerMismatch);
        }
        if self.sequence == 0 {
            return Err(EnrollmentError::RevisionSequenceNotPositive);
        }
        if self.sequence > i64::MAX as u64 {
            return Err(EnrollmentError::RevisionSequenceOverflow);
        }
        if self.authorization_expires_at != decision.authorization_expires_at {
            return Err(EnrollmentError::RevisionLeaseMismatch);
        }
        if self.revision_id
            != derive_revision_id(
                &self.network_id,
                &self.member_did,
                self.sequence,
                &self.kind,
                &self.request_digest,
            )
        {
            return Err(EnrollmentError::RevisionIdMismatch);
        }
        match self.kind {
            AuthorizationRevisionKind::Active => {
                if self.sequence != decision.authorization_sequence {
                    return Err(EnrollmentError::ActiveSequenceMismatch);
                }
                if self.issued_at != decision.decided_at {
                    return Err(EnrollmentError::ActiveTimestampMismatch);
                }
            }
            AuthorizationRevisionKind::Revoked => {
                if self.sequence <= decision.authorization_sequence {
                    return Err(EnrollmentError::RevocationNotDominant);
                }
                let decision_time =
                    parse_canonical_timestamp("decision decided_at", &decision.decided_at)?;
                let revision_time =
                    parse_canonical_timestamp("revision issued_at", &self.issued_at)?;
                if revision_time < decision_time {
                    return Err(EnrollmentError::RevocationBeforeApproval);
                }
            }
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::RevisionSignatureLength);
        }
        parse_canonical_timestamp("revision issued_at", &self.issued_at)?;
        Ok(())
    }
}

pub fn derive_revision_id(
    network_id: &str,
    member_did: &str,
    sequence: u64,
    kind: &AuthorizationRevisionKind,
    request_digest: &str,
) -> String {
    let sequence = sequence.to_string();
    format!(
        "authorization-{}",
        derive_enrollment_id(
            "gents-network-authorization-id-v1",
            &[
                network_id,
                member_did,
                &sequence,
                kind.as_str(),
                request_digest
            ],
        )
    )
}
