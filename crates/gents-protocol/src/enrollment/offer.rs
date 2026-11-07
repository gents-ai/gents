use std::io::Cursor;

use serde::{Deserialize, Serialize};

use super::error::{EnrollmentError, Result};
use super::{
    canonical::{canonical_domain_payload, validate_exact_field},
    ENROLLMENT_PROTOCOL_VERSION, MAX_LINKABLE_OFFER_CHARACTERS, MAX_OFFER_TOKEN_BYTES,
    OFFER_SIGNATURE_DOMAIN,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_schema_digest: Option<String>,
    pub issued_at: String,
    pub expires_at: String,
    pub admin_sig: Vec<u8>,
}

impl EnrollmentOfferRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        let mut fields = vec![
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
        ];
        if let Some(digest) = self.live_schema_digest.as_deref() {
            fields.push("live-schema-v1");
            fields.push(digest);
        }
        canonical_domain_payload(OFFER_SIGNATURE_DOMAIN, fields)
    }
}

pub fn linkable_offer_length(length: usize) -> bool {
    length > 0 && length <= MAX_LINKABLE_OFFER_CHARACTERS
}

pub fn encode_offer(offer: &EnrollmentOfferRecord) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(offer, &mut bytes).map_err(EnrollmentError::EncodeOffer)?;
    let encoded = bs58::encode(bytes).into_string();
    if encoded.len() > MAX_OFFER_TOKEN_BYTES {
        return Err(EnrollmentError::OfferTooLarge);
    }
    Ok(encoded)
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
    if let Some(digest) = &offer.live_schema_digest {
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or(EnrollmentError::InvalidLiveSchemaDigest)?;
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(EnrollmentError::InvalidLiveSchemaDigest);
        }
    }
    if offer.admin_sig.len() != 64 {
        return Err(EnrollmentError::OfferSignatureLength);
    }
    Ok(offer)
}

#[cfg(test)]
mod link_tests {
    use super::*;

    #[test]
    fn link_lengths_match_the_executable_model() {
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["enrollment_invite_length_cases"]
            .as_array()
            .unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let length = case["length"].as_u64().unwrap() as usize;
            assert_eq!(
                linkable_offer_length(length),
                case["linkable"].as_bool().unwrap(),
                "length {length}"
            );
        }
    }

    #[test]
    fn minted_token_is_url_safe_and_bounded() {
        let offer = EnrollmentOfferRecord {
            version: ENROLLMENT_PROTOCOL_VERSION,
            offer_id: "offer".into(),
            challenge: "challenge".into(),
            network_id: "network".into(),
            admin_did: "did:key:admin".into(),
            server_peer: "peer".into(),
            server_ticket: "t".repeat(400),
            owner_agent: "did:key:owner".into(),
            profile: "client".into(),
            schema_fingerprint: "schema".into(),
            live_schema_digest: None,
            issued_at: "2030-01-01T00:00:00Z".into(),
            expires_at: "2030-01-01T01:00:00Z".into(),
            admin_sig: vec![1; 64],
        };
        let token = encode_offer(&offer).unwrap();
        assert!(token.len() <= MAX_LINKABLE_OFFER_CHARACTERS);
        assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert_eq!(decode_offer(&token).unwrap(), offer);
        let mut with_schema = offer.clone();
        with_schema.live_schema_digest = Some(format!("sha256:{}", "a".repeat(64)));
        assert_ne!(offer.signing_payload(), with_schema.signing_payload());
        let with_schema_token = encode_offer(&with_schema).unwrap();
        assert_eq!(decode_offer(&with_schema_token).unwrap(), with_schema);
        for invalid in ["", "sha256:abc", &format!("sha256:{}", "A".repeat(64))] {
            let mut malformed = with_schema.clone();
            malformed.live_schema_digest = Some(invalid.to_owned());
            let token = encode_offer(&malformed).unwrap();
            assert!(matches!(
                decode_offer(&token),
                Err(EnrollmentError::InvalidLiveSchemaDigest)
            ));
        }
        let mut large_status_offer = offer.clone();
        large_status_offer.server_ticket = "t".repeat(3000);
        let status_token = encode_offer(&large_status_offer).unwrap();
        assert!(!linkable_offer_length(status_token.len()));
        assert_eq!(decode_offer(&status_token).unwrap(), large_status_offer);
        let mut oversized = offer;
        oversized.server_ticket = "t".repeat(MAX_OFFER_TOKEN_BYTES);
        assert!(matches!(
            encode_offer(&oversized),
            Err(EnrollmentError::OfferTooLarge)
        ));
    }
}
