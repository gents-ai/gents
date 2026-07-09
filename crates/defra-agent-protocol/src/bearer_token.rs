//! Audience-unbound bearer pairing invite (`dabear1-`) and its claim record.
//!
//! A bearer invite is the scan-one-QR counterpart of the DID-bound `dapair1-`
//! invite: it carries the issuer's identity, transport ticket, a single-use
//! nonce, and a scope template — but **no membership grant**, because the
//! claiming device's DID is unknown at mint. The claimant binds itself at
//! claim time by writing a self-signed [`BearerClaimRecord`] that replicates
//! to the issuer; the issuer's bearer-claim reconciler validates both
//! signatures, burns the nonce in its own `ConsumedInviteNonce` ledger, and
//! authors the `NetworkMembership` (plus the `ReciprocalConversationIntent`
//! for `conversation` templates).
//!
//! Deliberately a distinct type and prefix rather than an `InviteToken`
//! version bump: the DID-bound v5 join path is untouched, and consumers
//! dispatch on the prefix. Mirrors the [`crate::network_token::NetworkPointer`]
//! precedent ("carries no grant; the admin authors the actual membership").
//!
//! Modeled in `Proofs/PeerRegistryDiscovery/BearerClaim.lean`: admission
//! requires the issuer signature over the token AND the claimant signature
//! over the claim, token freshness, and the nonce not already bound to a
//! different claimant. The nonce ledger lives on the **authority**, not the
//! joiner — bearer tokens are exactly the case where two devices can race one
//! nonce.

use std::io::Cursor;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::network_token::NetworkRecord;
use crate::pairing_token::check_issued_at_freshness;

/// Audience-unbound pairing invite. CBOR-encoded, bs58-encoded, prefixed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BearerInviteToken {
    pub v: u8,
    pub issuer_did: String,
    pub peer_id: String,
    /// Issuer's dialable shareable address (the QR carries connectivity).
    pub ticket: String,
    /// Single-use claim nonce. Burned in the ISSUER's `ConsumedInviteNonce`
    /// ledger at claim-processing time, bound to the admitted claimant DID.
    pub nonce: String,
    pub network_id: String,
    pub issued_at: String,
    /// Scope template id (e.g. `"conversation"`). Determines the claim's
    /// consequences: `conversation` claims also record a
    /// `ReciprocalConversationIntent` for the claimant.
    pub template: String,
    /// Admin-signed network root record, so the claimant can TOFU-pin the
    /// network identity it is joining before writing anything.
    pub network: NetworkRecord,
    /// Issuer DID's signature over [`bearer_signing_payload`].
    pub sig: Vec<u8>,
}

/// Prefix for all encoded bearer invite tokens.
pub const BEARER_TOKEN_PREFIX: &str = "dabear1-";
/// Current bearer-token version.
pub const BEARER_TOKEN_VERSION: u8 = 1;

/// Default maximum age of a bearer invite before claims on it are rejected.
///
/// Much tighter than the DID-bound invite's 1h window: a bearer QR is
/// claimable by whoever scans it, so the replay window is the whole exposure.
/// QR flows are interactive — mint, scan, claim within minutes.
pub const BEARER_INVITE_MAX_AGE: Duration = Duration::minutes(5);

/// Verify a bearer token's signed `issued_at` against the bearer replay
/// window ([`BEARER_INVITE_MAX_AGE`]), evaluated at claim-processing time.
pub fn check_bearer_freshness(token: &BearerInviteToken, now: DateTime<Utc>) -> Result<()> {
    check_issued_at_freshness(&token.issued_at, now, BEARER_INVITE_MAX_AGE)
}

/// Encode a bearer token as `BEARER_TOKEN_PREFIX` + base58(CBOR(token)).
pub fn encode_bearer(token: &BearerInviteToken) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(token, &mut bytes).context("encoding bearer invite token")?;
    Ok(format!(
        "{BEARER_TOKEN_PREFIX}{}",
        bs58::encode(bytes).into_string()
    ))
}

/// Decode a `BEARER_TOKEN_PREFIX`-prefixed bearer token string. Rejects any
/// version other than [`BEARER_TOKEN_VERSION`] with a re-issue hint.
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
             re-issue with a newer defra-agent"
        ),
    }
}

/// CBOR of the token with `sig` zeroed — the bytes the issuer signs and the
/// claim processor verifies. Covers every field including `v` (downgrade
/// guard), `nonce`, `network_id`, `template`, and `network`.
pub fn bearer_signing_payload(token: &BearerInviteToken) -> Vec<u8> {
    let mut unsigned = token.clone();
    unsigned.sig = Vec::new();
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&unsigned, &mut bytes)
        .expect("CBOR serialisation of signing payload is infallible for valid BearerInviteToken");
    bytes
}

/// Canonical signing form of a `PairingBearerClaim` row: the claimant's
/// self-signed redemption of a bearer token. `sig` is the claimant DID's
/// signature over [`signing_payload`](BearerClaimRecord::signing_payload).
///
/// The claim row grants nothing by itself (mirrors the Lean
/// `unsigned_claim_grants_nothing` / `join_request_grants_nothing`
/// obligations): authority lives in the issuer-side claim reconciler, which
/// verifies the embedded token's issuer signature and this record's claimant
/// signature before authoring any membership.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BearerClaimRecord {
    /// The full encoded `dabear1-` token being redeemed. Embedding the token
    /// keeps the issuer stateless at mint: the token is self-authenticating to
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
    /// CBOR of this record with `sig` zeroed — the bytes signed/verified.
    /// Mirrors [`crate::pairing_token::signing_payload`].
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.sig = Vec::new();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&unsigned, &mut bytes)
            .expect("CBOR serialisation of signing payload is infallible");
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
