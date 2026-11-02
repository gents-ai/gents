use serde::{Deserialize, Serialize};

use super::canonical::{canonical_domain_payload, derive_enrollment_id, parse_canonical_timestamp};
use super::decision::{EnrollmentDecisionKind, EnrollmentDecisionRecord};
use super::error::{EnrollmentError, Result};
use super::request::EnrollmentRequestRecord;
use super::{ENROLLMENT_PROTOCOL_VERSION, ROUTE_RECEIPT_SIGNATURE_DOMAIN};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentRouteReceiptRecord {
    pub protocol_version: u8,
    pub receipt_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub member_did: String,
    pub member_peer: String,
    pub server_peer: String,
    pub owner_agent: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
    pub direction: EnrollmentRouteReceiptDirection,
    pub applied_at: String,
    pub signer_did: String,
    pub admin_sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentRouteReceiptDirection {
    ClientToServer,
}

impl EnrollmentRouteReceiptDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClientToServer => "client_to_server",
        }
    }
}

impl EnrollmentRouteReceiptRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        let sequence = self.authorization_sequence.to_string();
        canonical_domain_payload(
            ROUTE_RECEIPT_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.receipt_id,
                &self.request_id,
                &self.request_digest,
                &self.network_id,
                &self.admin_did,
                &self.member_did,
                &self.member_peer,
                &self.server_peer,
                &self.owner_agent,
                sequence.as_str(),
                &self.authorization_expires_at,
                self.direction.as_str(),
                &self.applied_at,
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
            return Err(EnrollmentError::UnsupportedRouteReceiptVersion {
                version: self.protocol_version,
            });
        }
        decision.validate_against_request(request)?;
        if decision.decision != EnrollmentDecisionKind::Approved {
            return Err(EnrollmentError::RouteReceiptNotApproved);
        }
        if self.request_id != request.request_id {
            return Err(EnrollmentError::ReceiptRequestIdMismatch);
        }
        if self.request_digest != request.request_digest {
            return Err(EnrollmentError::ReceiptRequestDigestMismatch);
        }
        if self.network_id != request.network_id {
            return Err(EnrollmentError::ReceiptNetworkIdMismatch);
        }
        if self.admin_did != request.admin_did {
            return Err(EnrollmentError::ReceiptAdminDidMismatch);
        }
        if self.member_did != request.candidate_did {
            return Err(EnrollmentError::ReceiptMemberDidMismatch);
        }
        if self.member_peer != request.candidate_peer {
            return Err(EnrollmentError::ReceiptMemberPeerMismatch);
        }
        if self.server_peer != request.server_peer {
            return Err(EnrollmentError::ReceiptServerPeerMismatch);
        }
        if self.owner_agent != request.owner_agent {
            return Err(EnrollmentError::ReceiptOwnerAgentMismatch);
        }
        if self.authorization_sequence != decision.authorization_sequence {
            return Err(EnrollmentError::ReceiptSequenceMismatch);
        }
        if self.authorization_expires_at != decision.authorization_expires_at {
            return Err(EnrollmentError::ReceiptLeaseMismatch);
        }
        if self.signer_did != request.admin_did {
            return Err(EnrollmentError::ReceiptSignerMismatch);
        }
        if self.receipt_id
            != derive_route_receipt_id(
                &self.request_id,
                &self.request_digest,
                self.authorization_sequence,
                &self.direction,
            )
        {
            return Err(EnrollmentError::ReceiptIdMismatch);
        }
        let decided = parse_canonical_timestamp("decision decided_at", &decision.decided_at)?;
        let applied = parse_canonical_timestamp("receipt applied_at", &self.applied_at)?;
        if applied < decided {
            return Err(EnrollmentError::RouteReceiptBeforeApproval);
        }
        let expires = parse_canonical_timestamp(
            "receipt authorization_expires_at",
            &self.authorization_expires_at,
        )?;
        if applied >= expires {
            return Err(EnrollmentError::RouteReceiptAfterLease);
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::RouteReceiptSignatureLength);
        }
        Ok(())
    }
}

pub fn derive_route_receipt_id(
    request_id: &str,
    request_digest: &str,
    authorization_sequence: u64,
    direction: &EnrollmentRouteReceiptDirection,
) -> String {
    let sequence = authorization_sequence.to_string();
    format!(
        "route-receipt-{}",
        derive_enrollment_id(
            "gents-enrollment-route-receipt-id-v1",
            &[request_id, request_digest, &sequence, direction.as_str()],
        )
    )
}
