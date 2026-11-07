use std::io::Cursor;

use serde::{Deserialize, Serialize};

use super::error::{EnrollmentError, Result};
use super::{
    canonical::{canonical_domain_payload, validate_exact_field},
    ENROLLMENT_PROTOCOL_VERSION, MAX_OFFER_TOKEN_BYTES, OFFER_SIGNATURE_DOMAIN,
};

const RENEWAL_CHALLENGE_DOMAIN: &str = "gents-enrollment-renewal-challenge-v1";
const RENEWAL_CHALLENGE_PREFIX: &str = "renewal-v1";

/// The signed offer challenge binds a renewal to one member and authorization
/// generation. The nonce must be fresh for each offer.
pub fn renewal_challenge(
    network: &str,
    member: &str,
    peer: &str,
    predecessor_sequence: u64,
    nonce: &str,
) -> Option<String> {
    if network.is_empty()
        || member.is_empty()
        || peer.is_empty()
        || predecessor_sequence == 0
        || nonce.len() != 32
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let sequence = predecessor_sequence.to_string();
    let binding = super::derive_enrollment_id(
        RENEWAL_CHALLENGE_DOMAIN,
        &[network, member, peer, &sequence, nonce],
    );
    Some(format!("{RENEWAL_CHALLENGE_PREFIX}-{nonce}-{binding}"))
}

/// Call only after verifying the offer signature and the member request.
pub fn renewal_challenge_matches(
    challenge: &str,
    network: &str,
    member: &str,
    peer: &str,
    predecessor_sequence: u64,
) -> bool {
    let Some(rest) = challenge.strip_prefix("renewal-v1-") else {
        return false;
    };
    let Some((nonce, _)) = rest.split_once('-') else {
        return false;
    };
    renewal_challenge(network, member, peer, predecessor_sequence, nonce)
        .is_some_and(|expected| expected == challenge)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentOfferRecord {
    pub version: u8,
    pub offer_id: String,
    pub challenge: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub server_ticket: String,
    pub owner_agent: String,
    pub profile: String,
    pub schema_fingerprint: String,
    pub issued_at: String,
    pub expires_at: String,
    pub admin_sig: Vec<u8>,
}

impl EnrollmentOfferRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        canonical_domain_payload(
            OFFER_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.offer_id,
                &self.challenge,
                &self.network_id,
                &self.admin_did,
                &self.server_peer,
                &self.server_ticket,
                &self.owner_agent,
                &self.profile,
                &self.schema_fingerprint,
                &self.issued_at,
                &self.expires_at,
            ],
        )
    }
}

pub fn encode_offer(offer: &EnrollmentOfferRecord) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(offer, &mut bytes).map_err(EnrollmentError::EncodeOffer)?;
    Ok(bs58::encode(bytes).into_string())
}

pub fn decode_offer(encoded: &str) -> Result<EnrollmentOfferRecord> {
    if encoded.is_empty() {
        return Err(EnrollmentError::OfferEmpty);
    }
    if encoded != encoded.trim() {
        return Err(EnrollmentError::OfferWhitespace);
    }
    if encoded.len() > MAX_OFFER_TOKEN_BYTES {
        return Err(EnrollmentError::OfferTooLarge);
    }
    let bytes = bs58::decode(encoded)
        .into_vec()
        .map_err(EnrollmentError::DecodeOfferBase58)?;
    if bytes.len() > MAX_OFFER_TOKEN_BYTES {
        return Err(EnrollmentError::DecodedOfferTooLarge);
    }
    let mut cursor = Cursor::new(bytes.as_slice());
    let offer: EnrollmentOfferRecord =
        ciborium::de::from_reader(&mut cursor).map_err(EnrollmentError::ParseOffer)?;
    if cursor.position() != bytes.len() as u64 {
        return Err(EnrollmentError::OfferTrailingData);
    }
    if offer.version != ENROLLMENT_PROTOCOL_VERSION {
        return Err(EnrollmentError::UnsupportedOfferVersion {
            version: offer.version,
        });
    }
    for (name, value) in [
        ("offer_id", offer.offer_id.as_str()),
        ("challenge", offer.challenge.as_str()),
        ("network_id", offer.network_id.as_str()),
        ("admin_did", offer.admin_did.as_str()),
        ("server_peer", offer.server_peer.as_str()),
        ("server_ticket", offer.server_ticket.as_str()),
        ("owner_agent", offer.owner_agent.as_str()),
        ("profile", offer.profile.as_str()),
        ("schema_fingerprint", offer.schema_fingerprint.as_str()),
        ("issued_at", offer.issued_at.as_str()),
        ("expires_at", offer.expires_at.as_str()),
    ] {
        validate_exact_field(name, value)?;
    }
    if offer.profile != "client" {
        return Err(EnrollmentError::UnsupportedOfferProfile);
    }
    if offer.admin_sig.len() != 64 {
        return Err(EnrollmentError::OfferSignatureLength);
    }
    Ok(offer)
}

#[cfg(test)]
mod renewal_tests {
    use super::{renewal_challenge, renewal_challenge_matches};

    #[test]
    fn challenge_binds_member_peer_network_and_generation() {
        let nonce = "0123456789abcdef0123456789abcdef";
        let challenge = renewal_challenge("network", "member", "peer", 3, nonce).unwrap();
        assert!(renewal_challenge_matches(
            &challenge, "network", "member", "peer", 3
        ));
        for (network, member, peer, sequence) in [
            ("other", "member", "peer", 3),
            ("network", "other", "peer", 3),
            ("network", "member", "other", 3),
            ("network", "member", "peer", 4),
        ] {
            assert!(!renewal_challenge_matches(
                &challenge, network, member, peer, sequence
            ));
        }
        assert!(!renewal_challenge_matches(
            "ordinary-offer",
            "network",
            "member",
            "peer",
            3
        ));
        assert!(renewal_challenge("network", "member", "peer", 0, nonce).is_none());
        assert!(renewal_challenge("network", "", "peer", 3, nonce).is_none());
        assert!(renewal_challenge("network", "member", "peer", 3, "short").is_none());
    }
}
