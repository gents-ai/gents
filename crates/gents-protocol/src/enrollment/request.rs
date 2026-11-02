use serde::{Deserialize, Serialize};

use super::canonical::{
    canonical_domain_payload, canonical_enrollment_digest, derive_enrollment_id,
    parse_canonical_timestamp, validate_exact_field,
};
use super::error::{EnrollmentError, Result};
use super::offer::EnrollmentOfferRecord;
use super::{ENROLLMENT_PROTOCOL_VERSION, REQUEST_SIGNATURE_DOMAIN};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentRequestRecord {
    pub protocol_version: u8,
    pub request_id: String,
    pub request_digest: String,
    pub offer_id: String,
    pub offer_token: String,
    pub challenge: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub candidate_ticket: String,
    pub owner_agent: String,
    pub profile: String,
    pub client_nonce: String,
    pub issued_at: String,
    pub expires_at: String,
    pub candidate_sig: Vec<u8>,
}

impl EnrollmentRequestRecord {
    pub fn canonical_text_fields(&self) -> [&str; 13] {
        [
            &self.request_id,
            &self.offer_id,
            &self.challenge,
            &self.network_id,
            &self.admin_did,
            &self.server_peer,
            &self.candidate_did,
            &self.candidate_peer,
            &self.owner_agent,
            &self.profile,
            &self.client_nonce,
            &self.issued_at,
            &self.expires_at,
        ]
    }

    pub fn computed_digest(&self) -> String {
        canonical_enrollment_digest(self.canonical_text_fields())
    }

    pub fn validate_against_offer(&self, offer: &EnrollmentOfferRecord) -> Result<()> {
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedRequestVersion {
                version: self.protocol_version,
            });
        }
        if self.offer_id != offer.offer_id {
            return Err(EnrollmentError::RequestOfferIdMismatch);
        }
        if self.challenge != offer.challenge {
            return Err(EnrollmentError::RequestChallengeMismatch);
        }
        if self.network_id != offer.network_id {
            return Err(EnrollmentError::RequestNetworkIdMismatch);
        }
        if self.admin_did != offer.admin_did {
            return Err(EnrollmentError::RequestAdminDidMismatch);
        }
        if self.server_peer != offer.server_peer {
            return Err(EnrollmentError::RequestServerPeerMismatch);
        }
        if self.owner_agent != offer.owner_agent {
            return Err(EnrollmentError::RequestOwnerAgentMismatch);
        }
        if self.profile != offer.profile {
            return Err(EnrollmentError::RequestProfileMismatch);
        }
        if self.expires_at != offer.expires_at {
            return Err(EnrollmentError::RequestExtendsOffer);
        }
        if self.request_digest != self.computed_digest() {
            return Err(EnrollmentError::RequestDigestMismatch);
        }
        let expected_id = format!(
            "enroll-{}",
            derive_enrollment_id(
                "gents-enrollment-request-id-v1",
                &[
                    &self.offer_id,
                    &self.candidate_did,
                    &self.candidate_peer,
                    &self.client_nonce,
                ],
            )
        );
        if self.request_id != expected_id {
            return Err(EnrollmentError::RequestIdMismatch);
        }
        for (name, value) in [
            ("request_id", self.request_id.as_str()),
            ("request_digest", self.request_digest.as_str()),
            ("offer_id", self.offer_id.as_str()),
            ("offer_token", self.offer_token.as_str()),
            ("challenge", self.challenge.as_str()),
            ("network_id", self.network_id.as_str()),
            ("admin_did", self.admin_did.as_str()),
            ("server_peer", self.server_peer.as_str()),
            ("candidate_did", self.candidate_did.as_str()),
            ("candidate_peer", self.candidate_peer.as_str()),
            ("candidate_ticket", self.candidate_ticket.as_str()),
            ("owner_agent", self.owner_agent.as_str()),
            ("profile", self.profile.as_str()),
            ("client_nonce", self.client_nonce.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("expires_at", self.expires_at.as_str()),
        ] {
            validate_exact_field(name, value)?;
        }
        if self.candidate_sig.len() != 64 {
            return Err(EnrollmentError::RequestSignatureLength);
        }
        let offer_issued = parse_canonical_timestamp("offer issued_at", &offer.issued_at)?;
        let request_issued = parse_canonical_timestamp("request issued_at", &self.issued_at)?;
        let offer_expires = parse_canonical_timestamp("offer expires_at", &offer.expires_at)?;
        if !(offer_issued <= request_issued && request_issued <= offer_expires) {
            return Err(EnrollmentError::RequestOutsideOfferWindow);
        }
        Ok(())
    }
}

impl EnrollmentRequestRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        canonical_domain_payload(
            REQUEST_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.request_id,
                &self.request_digest,
                &self.offer_id,
                &self.offer_token,
                &self.challenge,
                &self.network_id,
                &self.admin_did,
                &self.server_peer,
                &self.candidate_did,
                &self.candidate_peer,
                &self.candidate_ticket,
                &self.owner_agent,
                &self.profile,
                &self.client_nonce,
                &self.issued_at,
                &self.expires_at,
            ],
        )
    }
}
