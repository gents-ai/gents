use serde::{Deserialize, Serialize};

use super::canonical::{
    canonical_domain_payload, canonical_enrollment_digest, derive_enrollment_id,
    parse_canonical_timestamp, validate_exact_field,
};
use super::error::{EnrollmentError, Result};
use super::offer::EnrollmentOfferRecord;
use super::{ENROLLMENT_PROTOCOL_VERSION, REQUEST_SIGNATURE_DOMAIN};

pub fn candidate_name_fields(name: Option<&str>) -> Vec<&str> {
    match name {
        None => Vec::new(),
        Some(name) => vec!["candidate-name-v1", name],
    }
}

#[cfg(test)]
mod candidate_name_conformance {
    use super::{candidate_name_fields, EnrollmentRequestRecord};

    #[test]
    fn fields_match_the_executable_model() {
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["enrollment_candidate_name_cases"]
            .as_array()
            .unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let expected = case["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|field| field.as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                candidate_name_fields(case["name"].as_str()),
                expected,
                "{case}"
            );
        }
    }

    #[test]
    fn name_changes_bind_both_the_digest_and_signature_payload() {
        let mut request = EnrollmentRequestRecord {
            protocol_version: 2,
            request_id: "request".into(),
            request_digest: "digest".into(),
            offer_id: "offer".into(),
            offer_token: "token".into(),
            challenge: "challenge".into(),
            network_id: "network".into(),
            admin_did: "admin".into(),
            server_peer: "server".into(),
            candidate_did: "candidate".into(),
            candidate_peer: "peer".into(),
            candidate_name: None,
            candidate_ticket: "ticket".into(),
            owner_agent: "owner".into(),
            profile: "client".into(),
            client_nonce: "nonce".into(),
            issued_at: "issued".into(),
            expires_at: "expires".into(),
            candidate_sig: Vec::new(),
        };
        let unnamed_digest = request.computed_digest();
        let unnamed_payload = request.signing_payload();
        request.candidate_name = Some("Ivan".into());
        assert_ne!(request.computed_digest(), unnamed_digest);
        assert_ne!(request.signing_payload(), unnamed_payload);
        let named_digest = request.computed_digest();
        let named_payload = request.signing_payload();
        request.candidate_name = Some("Other".into());
        assert_ne!(request.computed_digest(), named_digest);
        assert_ne!(request.signing_payload(), named_payload);
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_name: Option<String>,
    pub candidate_ticket: String,
    pub owner_agent: String,
    pub profile: String,
    pub client_nonce: String,
    pub issued_at: String,
    pub expires_at: String,
    pub candidate_sig: Vec<u8>,
}

impl EnrollmentRequestRecord {
    pub fn canonical_text_fields(&self) -> Vec<&str> {
        let mut fields: Vec<&str> = vec![
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
        ];
        fields.extend(candidate_name_fields(self.candidate_name.as_deref()));
        fields
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
        if let Some(name) = &self.candidate_name {
            validate_exact_field("candidate_name", name)?;
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
        let mut fields: Vec<&str> = vec![
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
        ];
        fields.extend(candidate_name_fields(self.candidate_name.as_deref()));
        canonical_domain_payload(REQUEST_SIGNATURE_DOMAIN, fields)
    }
}
