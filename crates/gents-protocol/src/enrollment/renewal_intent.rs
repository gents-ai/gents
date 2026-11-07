use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::canonical::{canonical_domain_payload, parse_canonical_timestamp, validate_exact_field};
use super::error::{EnrollmentError, Result};

const INTENT_DOMAIN: &str = "gents-enrollment-renewal-intent-v1";
const OFFER_RESPONSE_DOMAIN: &str = "gents-enrollment-renewal-offer-response-v1";
pub const RENEWAL_INTENT_VALIDITY_SECONDS: i64 = 300;

/// A member-signed request for an offer; it does not itself grant access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewalIntent {
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub member_did: String,
    pub member_peer: String,
    pub predecessor_sequence: u64,
    pub issued_at: String,
    pub expires_at: String,
    pub nonce: String,
    pub member_sig: Vec<u8>,
}

/// An owner-signed response to exactly one member's renewal intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewalOfferResponse {
    pub intent_id: String,
    pub member_did: String,
    pub offer_token: String,
    pub admin_did: String,
    pub admin_sig: Vec<u8>,
}

impl RenewalOfferResponse {
    pub fn signing_payload(&self) -> Vec<u8> {
        canonical_domain_payload(
            OFFER_RESPONSE_DOMAIN,
            [
                self.intent_id.as_str(),
                self.member_did.as_str(),
                self.offer_token.as_str(),
                self.admin_did.as_str(),
            ],
        )
    }

    /// The caller must authenticate both signatures before consuming the offer.
    pub fn validate_for(&self, intent: &RenewalIntent, now: DateTime<Utc>) -> Result<()> {
        intent.validate_at(now)?;
        if self.intent_id != intent.intent_id()
            || self.member_did != intent.member_did
            || self.admin_did != intent.admin_did
            || self.admin_sig.len() != 64
        {
            return Err(EnrollmentError::RenewalIntentInvalid);
        }
        let offer = super::decode_offer(&self.offer_token)?;
        if offer.network_id != intent.network_id
            || offer.admin_did != intent.admin_did
            || offer.server_peer != intent.server_peer
            || !super::renewal_challenge_matches(
                &offer.challenge,
                &intent.network_id,
                &intent.member_did,
                &intent.member_peer,
                intent.predecessor_sequence,
            )
        {
            return Err(EnrollmentError::RenewalIntentInvalid);
        }
        Ok(())
    }
}

impl RenewalIntent {
    pub fn intent_id(&self) -> String {
        format!(
            "renewal-{}",
            super::derive_enrollment_id(
                "gents-enrollment-renewal-intent-id-v1",
                &[
                    &self.network_id,
                    &self.admin_did,
                    &self.server_peer,
                    &self.member_did,
                    &self.member_peer,
                    &self.predecessor_sequence.to_string(),
                    &self.issued_at,
                    &self.expires_at,
                    &self.nonce
                ],
            )
        )
    }

    pub fn signing_payload(&self) -> Vec<u8> {
        let sequence = self.predecessor_sequence.to_string();
        canonical_domain_payload(
            INTENT_DOMAIN,
            [
                &self.network_id,
                &self.admin_did,
                &self.server_peer,
                &self.member_did,
                &self.member_peer,
                &sequence,
                &self.issued_at,
                &self.expires_at,
                &self.nonce,
            ]
            .into_iter()
            .map(String::as_str),
        )
    }

    /// Signature authenticity and transport identity are checked by the owner.
    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<()> {
        for (name, value) in [
            ("network_id", self.network_id.as_str()),
            ("admin_did", self.admin_did.as_str()),
            ("server_peer", self.server_peer.as_str()),
            ("member_did", self.member_did.as_str()),
            ("member_peer", self.member_peer.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            validate_exact_field(name, value)?;
            if value.chars().any(char::is_control) {
                return Err(EnrollmentError::RenewalIntentInvalid);
            }
        }
        if self.predecessor_sequence == 0
            || self.predecessor_sequence > i64::MAX as u64
            || self.member_sig.len() != 64
        {
            return Err(EnrollmentError::RenewalIntentInvalid);
        }
        let issued = parse_canonical_timestamp("renewal intent issued_at", &self.issued_at)?
            .with_timezone(&Utc);
        let expires = parse_canonical_timestamp("renewal intent expires_at", &self.expires_at)?
            .with_timezone(&Utc);
        if issued > now
            || now >= expires
            || expires <= issued
            || expires - issued > Duration::seconds(RENEWAL_INTENT_VALIDITY_SECONDS)
        {
            return Err(EnrollmentError::RenewalIntentInvalid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> RenewalIntent {
        RenewalIntent {
            network_id: "network".into(),
            admin_did: "admin".into(),
            server_peer: "server".into(),
            member_did: "member".into(),
            member_peer: "peer".into(),
            predecessor_sequence: 1,
            issued_at: "2026-11-01T00:00:00Z".into(),
            expires_at: "2026-11-01T00:05:00Z".into(),
            nonce: "nonce".into(),
            member_sig: vec![0; 64],
        }
    }

    #[test]
    fn intent_window_and_signed_scope_are_exact() {
        let request = intent();
        assert!(request
            .validate_at("2026-11-01T00:00:00Z".parse().unwrap())
            .is_ok());
        assert!(request
            .validate_at("2026-11-01T00:05:00Z".parse().unwrap())
            .is_err());
        assert!(request
            .validate_at("2026-10-31T23:59:59Z".parse().unwrap())
            .is_err());
        let mut changed = request.clone();
        changed.member_peer = "other".into();
        assert_ne!(request.signing_payload(), changed.signing_payload());
        changed = request.clone();
        changed.predecessor_sequence = 2;
        assert_ne!(request.signing_payload(), changed.signing_payload());
        changed = request.clone();
        changed.expires_at = "2026-11-01T00:05:01Z".into();
        assert!(changed
            .validate_at("2026-11-01T00:01:00Z".parse().unwrap())
            .is_err());
    }

    #[test]
    fn response_binds_the_intent_and_offer_generation() {
        let request = intent();
        let challenge = super::super::renewal_challenge(
            &request.network_id,
            &request.member_did,
            &request.member_peer,
            request.predecessor_sequence,
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let offer = super::super::EnrollmentOfferRecord {
            version: super::super::ENROLLMENT_PROTOCOL_VERSION,
            offer_id: "offer".into(),
            challenge,
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            server_peer: request.server_peer.clone(),
            server_ticket: "ticket".into(),
            owner_agent: "owner".into(),
            profile: "client".into(),
            schema_fingerprint: "schema".into(),
            issued_at: request.issued_at.clone(),
            expires_at: request.expires_at.clone(),
            admin_sig: vec![0; 64],
        };
        let response = RenewalOfferResponse {
            intent_id: request.intent_id(),
            member_did: request.member_did.clone(),
            offer_token: super::super::encode_offer(&offer).unwrap(),
            admin_did: request.admin_did.clone(),
            admin_sig: vec![0; 64],
        };
        let now = "2026-11-01T00:01:00Z".parse().unwrap();
        assert!(response.validate_for(&request, now).is_ok());
        let mut changed = response.clone();
        changed.intent_id.push('x');
        assert!(changed.validate_for(&request, now).is_err());
        let mut changed_request = request.clone();
        changed_request.predecessor_sequence += 1;
        changed.intent_id = changed_request.intent_id();
        assert!(changed.validate_for(&changed_request, now).is_err());
    }
}
