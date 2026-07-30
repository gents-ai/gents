//! Audience-unbound bearer pairing invite (`dabear1-`) and its claim record.
//! nonce, and a scope template — but **no membership grant**, because the
//! signatures, burns the nonce in its own `ConsumedInviteNonce` ledger, and
//! requires the issuer signature over the token AND the claimant signature
//! over the claim, token freshness, and the nonce not already bound to a
//! different claimant. The nonce ledger lives on the **authority**, not the
//! joiner — bearer tokens are exactly the case where two devices can race one
//! nonce.

use std::io::Cursor;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::network_token::NetworkRecord;
use crate::pairing_token::check_issued_at_freshness;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BearerInviteToken {
    pub v: u8,
    pub issuer_did: String,
    pub peer_id: String,
    pub ticket: String,
    /// Single-use claim nonce. Burned in the ISSUER's `ConsumedInviteNonce`
    pub nonce: String,
    pub network_id: String,
    pub issued_at: String,
    pub template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_behavior_id: Option<String>,
    pub network: NetworkRecord,
    /// Issuer DID's signature over [`bearer_signing_payload`].
    pub sig: Vec<u8>,
}

pub const BEARER_TOKEN_PREFIX: &str = "dabear1-";
pub const BEARER_TOKEN_VERSION: u8 = 1;

pub const BEARER_INVITE_MAX_AGE: Duration = Duration::minutes(5);

pub fn check_bearer_freshness(token: &BearerInviteToken, now: DateTime<Utc>) -> Result<()> {
    check_issued_at_freshness(&token.issued_at, now, BEARER_INVITE_MAX_AGE)
}

pub fn encode_bearer(token: &BearerInviteToken) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(token, &mut bytes).context("encoding bearer invite token")?;
    Ok(format!(
        "{BEARER_TOKEN_PREFIX}{}",
        bs58::encode(bytes).into_string()
    ))
}

pub fn decode_bearer(raw: &str) -> Result<BearerInviteToken> {
    let encoded = raw
        .trim()
        .strip_prefix(BEARER_TOKEN_PREFIX)
        .context("invalid bearer invite token prefix")?;
    let bytes = bs58::decode(encoded)
        .into_vec()
        .context("decoding bearer invite token")?;
    let token: BearerInviteToken =
        ciborium::de::from_reader(Cursor::new(bytes)).context("parsing bearer invite token")?;
    match token.v {
        BEARER_TOKEN_VERSION => Ok(token),
        version => anyhow::bail!(
            "bearer invite token version {version} is not supported; \
             re-issue with a newer gents"
        ),
    }
}

/// guard), `nonce`, `network_id`, `template`, and `network`.
pub fn bearer_signing_payload(token: &BearerInviteToken) -> Vec<u8> {
    let mut unsigned = token.clone();
    unsigned.sig = Vec::new();
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&unsigned, &mut bytes)
        .expect("CBOR serialisation of signing payload is infallible for valid BearerInviteToken");
    bytes
}

/// signature over [`signing_payload`](BearerClaimRecord::signing_payload).
/// The claim row grants nothing by itself (mirrors the Lean
/// verifies the embedded token's issuer signature and this record's claimant
/// signature before authoring any membership.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BearerClaimRecord {
    /// its issuer, and revocation-before-claim is burning the nonce.
    pub token: String,
    pub claimant_did: String,
    pub claimant_node_id: String,
    pub claimant_address: String,
    pub claimed_at: String,
    /// Claimant DID's signature over the record with this field zeroed.
    pub sig: Vec<u8>,
}

impl BearerClaimRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.sig = Vec::new();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&unsigned, &mut bytes)
            .expect("CBOR serialisation of signing payload is infallible");
        bytes
    }
}

pub fn derive_bearer_readiness_key(issuer_did: &str, claimant_did: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(issuer_did.as_bytes());
    digest.update(b"\x1f");
    digest.update(claimant_did.as_bytes());
    let digest = digest.finalize();
    format!("ready-{}", bs58::encode(&digest[..16]).into_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BearerPairingReadyRecord {
    pub issuer_did: String,
    pub claimant_did: String,
    pub peer_id: String,
    pub address: String,
    pub template: String,
    pub acknowledged_at: String,
    /// Issuer DID's signature over the record with this field zeroed.
    pub sig: Vec<u8>,
}

impl BearerPairingReadyRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.sig = Vec::new();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&unsigned, &mut bytes)
            .expect("CBOR serialisation of bearer readiness is infallible");
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct LegacyBearerInviteToken {
        v: u8,
        issuer_did: String,
        peer_id: String,
        ticket: String,
        nonce: String,
        network_id: String,
        issued_at: String,
        template: String,
        network: NetworkRecord,
        sig: Vec<u8>,
    }

    fn sample_network() -> NetworkRecord {
        NetworkRecord {
            network_id: "default".into(),
            admin_did: "did:key:admin".into(),
            display_name: "Default".into(),
            default_template: "network-control".into(),
            created_at: "2026-07-08T00:00:00Z".into(),
            sig: vec![4, 5, 6],
        }
    }

    fn sample_bearer(v: u8) -> BearerInviteToken {
        BearerInviteToken {
            v,
            issuer_did: "did:key:issuer".into(),
            peer_id: "peer-issuer".into(),
            ticket: "/ticket/issuer".into(),
            nonce: "nonce-a".into(),
            network_id: "default".into(),
            issued_at: "2026-07-08T00:00:00Z".into(),
            template: "conversation".into(),
            default_behavior_id: Some("default".into()),
            network: sample_network(),
            sig: vec![1, 2, 3],
        }
    }

    #[test]
    fn bearer_token_round_trips_and_signing_payload_ignores_sig() {
        let t = sample_bearer(BEARER_TOKEN_VERSION);
        let enc = encode_bearer(&t).unwrap();
        assert!(enc.starts_with(BEARER_TOKEN_PREFIX));
        assert_eq!(decode_bearer(&enc).unwrap(), t);

        let mut t2 = t.clone();
        t2.sig = vec![9, 9, 9];
        assert_eq!(bearer_signing_payload(&t), bearer_signing_payload(&t2));
    }

    #[test]
    fn bearer_signing_payload_covers_nonce_template_and_network() {
        let a = sample_bearer(BEARER_TOKEN_VERSION);

        let mut b = a.clone();
        b.nonce = "nonce-b".into();
        assert_ne!(bearer_signing_payload(&a), bearer_signing_payload(&b));

        let mut b = a.clone();
        b.template = "network-control".into();
        assert_ne!(bearer_signing_payload(&a), bearer_signing_payload(&b));

        let mut b = a.clone();
        b.default_behavior_id = Some("review".into());
        assert_ne!(bearer_signing_payload(&a), bearer_signing_payload(&b));

        let mut b = a.clone();
        b.network.admin_did = "did:key:other".into();
        assert_ne!(bearer_signing_payload(&a), bearer_signing_payload(&b));

        let mut b = a.clone();
        b.v = 2;
        assert_ne!(
            bearer_signing_payload(&a),
            bearer_signing_payload(&b),
            "version must be signed (downgrade guard)"
        );
    }

    #[test]
    fn readiness_key_is_deterministic_and_party_bound() {
        let key = derive_bearer_readiness_key("did:key:issuer", "did:key:claimant");
        assert_eq!(
            key,
            derive_bearer_readiness_key("did:key:issuer", "did:key:claimant")
        );
        assert_ne!(
            key,
            derive_bearer_readiness_key("did:key:other", "did:key:claimant")
        );
        assert_ne!(
            key,
            derive_bearer_readiness_key("did:key:issuer", "did:key:other")
        );
    }

    #[test]
    fn readiness_signing_payload_covers_reciprocal_endpoint() {
        let record = BearerPairingReadyRecord {
            issuer_did: "did:key:issuer".into(),
            claimant_did: "did:key:claimant".into(),
            peer_id: "peer-claimant".into(),
            address: "ticket-claimant".into(),
            template: "conversation".into(),
            acknowledged_at: "2026-07-27T00:00:00Z".into(),
            sig: vec![1, 2, 3],
        };
        let mut resigned = record.clone();
        resigned.sig = vec![9, 9, 9];
        assert_eq!(
            record.signing_payload(),
            resigned.signing_payload(),
            "signature bytes are excluded"
        );

        let mut changed = record.clone();
        changed.address = "ticket-other".into();
        assert_ne!(record.signing_payload(), changed.signing_payload());
    }

    #[test]
    fn missing_behavior_hint_preserves_legacy_signing_payload() {
        let legacy = LegacyBearerInviteToken {
            v: BEARER_TOKEN_VERSION,
            issuer_did: "did:key:issuer".into(),
            peer_id: "peer-issuer".into(),
            ticket: "/ticket/issuer".into(),
            nonce: "nonce-a".into(),
            network_id: "default".into(),
            issued_at: "2026-07-08T00:00:00Z".into(),
            template: "conversation".into(),
            network: sample_network(),
            sig: Vec::new(),
        };
        let mut legacy_bytes = Vec::new();
        ciborium::ser::into_writer(&legacy, &mut legacy_bytes).unwrap();
        let encoded = format!(
            "{BEARER_TOKEN_PREFIX}{}",
            bs58::encode(&legacy_bytes).into_string()
        );

        let decoded = decode_bearer(&encoded).unwrap();

        assert_eq!(decoded.default_behavior_id, None);
        assert_eq!(bearer_signing_payload(&decoded), legacy_bytes);
    }

    #[test]
    fn decode_bearer_rejects_unknown_version_and_wrong_prefix() {
        let t = sample_bearer(9);
        let enc = encode_bearer(&t).unwrap();
        let err = decode_bearer(&enc).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "unexpected error: {err}"
        );

        let err = decode_bearer("dapair1-notbearer").unwrap_err().to_string();
        assert!(err.contains("invalid bearer invite token prefix"));
    }

    #[test]
    fn bearer_freshness_window_is_five_minutes() {
        let mut t = sample_bearer(BEARER_TOKEN_VERSION);
        let now = DateTime::parse_from_rfc3339("2026-07-08T00:10:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // 2 minutes old — fresh.
        t.issued_at = "2026-07-08T00:08:00Z".into();
        assert!(check_bearer_freshness(&t, now).is_ok());

        // 10 minutes old — expired.
        t.issued_at = "2026-07-08T00:00:00Z".into();
        assert!(check_bearer_freshness(&t, now).is_err());

        // 10 minutes in the future — rejected.
        t.issued_at = "2026-07-08T00:20:00Z".into();
        assert!(check_bearer_freshness(&t, now).is_err());
    }

    #[test]
    fn claim_record_signing_payload_covers_token_and_claimant() {
        let record = BearerClaimRecord {
            token: "dabear1-abc".into(),
            claimant_did: "did:key:phone".into(),
            claimant_node_id: "peer-phone".into(),
            claimant_address: "/ticket/phone".into(),
            claimed_at: "2026-07-08T00:01:00Z".into(),
            sig: vec![1],
        };

        let mut b = record.clone();
        b.token = "dabear1-def".into();
        assert_ne!(record.signing_payload(), b.signing_payload());

        let mut b = record.clone();
        b.claimant_did = "did:key:other".into();
        assert_ne!(record.signing_payload(), b.signing_payload());

        let mut b = record.clone();
        b.sig = vec![9, 9];
        assert_eq!(
            record.signing_payload(),
            b.signing_payload(),
            "payload excludes sig"
        );
    }
}
