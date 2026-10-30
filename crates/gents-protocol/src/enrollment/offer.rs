use std::io::Cursor;

use serde::{Deserialize, Serialize};

use super::error::{EnrollmentError, Result};
use super::{
    canonical::{canonical_domain_payload, validate_exact_field},
    ENROLLMENT_PROTOCOL_VERSION, MAX_OFFER_TOKEN_BYTES, OFFER_SIGNATURE_DOMAIN,
};

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
