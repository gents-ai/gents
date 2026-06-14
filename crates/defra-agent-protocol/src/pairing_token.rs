//! Signed pairing-invite token (current version: v3).
//!
//! The token carries everything the joining peer needs to connect plus a
//! signature over that payload by the issuer's DID key.  The join command
//! verifies the signature (TOFU) before writing a `PeerPairingDesired` row.
//!
//! Encoding: CBOR (ciborium) → base58 → `"dapair1-"` prefix.
//!
//! Signing payload: CBOR of a copy of the token with `sig = []`.  This means
//! the signature covers `v`, `issuer_did`, `peer_id`, `ticket`, `profiles`,
//! `network_id`, `issued_at`, and `template` — every field that matters for
//! correctness — while remaining stable across sig values.
//!
//! Version history:
//!   v2 — original release (profiles, no template)
//!   v3 — adds `template: String`; older tokens rejected with a re-issue hint

use std::io::Cursor;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Versioned pairing-invite envelope.  CBOR-encoded, bs58-encoded, prefixed.
///
/// Current version: v3.  The `template` field was added in v3; v2 tokens are
/// rejected on decode with a re-issue hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteToken {
    pub v: u8,
    pub issuer_did: String,
    pub peer_id: String,
    pub ticket: String,
    pub profiles: Vec<String>,
    pub network_id: String,
    pub issued_at: String,
    /// Scope template id (e.g. `"conversation"`) selected by the invite issuer.
    /// Added in v3; the `join` command writes this as the desired row's template.
    pub template: String,
    /// Ed25519 (or other) signature over `signing_payload(self)`.
    /// Empty when computing the payload itself (circular-dependency break).
    pub sig: Vec<u8>,
}

/// Prefix for all encoded invite tokens.
pub const TOKEN_PREFIX: &str = "dapair1-";

/// Encode a token as `TOKEN_PREFIX` + base58(CBOR(token)).
pub fn encode(token: &InviteToken) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(token, &mut bytes).context("encoding pairing invite token")?;
    Ok(format!(
        "{TOKEN_PREFIX}{}",
        bs58::encode(bytes).into_string()
    ))
}

/// Decode a `TOKEN_PREFIX`-prefixed invite token string.
///
/// Returns an error (mentioning "re-issue with a newer defra-agent") for any
/// token whose `v` field is not `3`.  v2 tokens (from before the `template`
/// field was added) are rejected so the issuer must re-mint with a current
/// `defra-agent` binary.
pub fn decode(raw: &str) -> Result<InviteToken> {
    let encoded = raw
        .trim()
        .strip_prefix(TOKEN_PREFIX)
        .context("invalid pairing invite token prefix")?;
    let bytes = bs58::decode(encoded)
        .into_vec()
        .context("decoding pairing invite token")?;
    let token: InviteToken =
        ciborium::de::from_reader(Cursor::new(bytes)).context("parsing pairing invite token")?;
    match token.v {
        3 => Ok(token),
        version => anyhow::bail!(
            "pairing invite token version {version} is not supported; \
             re-issue with a newer defra-agent"
        ),
    }
}

/// Compute the bytes that are signed/verified for a token.
///
/// Serialises a copy of the token with `sig` zeroed to an empty vec, so the
/// signature covers every other field (including `v`, guarding against version
/// downgrade replays) without a circularity.
pub fn signing_payload(token: &InviteToken) -> Vec<u8> {
    let mut unsigned = token.clone();
    unsigned.sig = Vec::new();
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&unsigned, &mut bytes)
        .expect("CBOR serialisation of signing payload is infallible for valid InviteToken");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_token(v: u8) -> InviteToken {
        InviteToken {
            v,
            issuer_did: "did:key:a".into(),
            peer_id: "p".into(),
            ticket: "/ip4/1".into(),
            profiles: vec!["chat-requests".into()],
            network_id: "default".into(),
            issued_at: "2026-06-13T00:00:00Z".into(),
            template: "conversation".into(),
            sig: vec![1, 2, 3],
        }
    }

    #[test]
    fn token_v3_round_trips_and_signing_payload_ignores_sig() {
        let t = InviteToken {
            v: 3,
            issuer_did: "did:key:a".into(),
            peer_id: "p".into(),
            ticket: "/ip4/1".into(),
            profiles: vec!["chat-requests".into()],
            network_id: "default".into(),
            issued_at: "2026-06-13T00:00:00Z".into(),
            template: "conversation".into(),
            sig: vec![1, 2, 3],
        };
        let enc = encode(&t).unwrap();
        assert!(enc.starts_with(TOKEN_PREFIX));
        assert_eq!(decode(&enc).unwrap(), t);
        let mut t2 = t.clone();
        t2.sig = vec![9, 9, 9];
        assert_eq!(signing_payload(&t), signing_payload(&t2)); // payload excludes sig
    }

    #[test]
    fn token_v3_signing_payload_covers_template() {
        // Two tokens identical except template → different signing payloads.
        let mut a = sample_token(3);
        a.template = "conversation".into();
        a.sig = vec![];
        let mut b = sample_token(3);
        b.template = "backup".into();
        b.sig = vec![];
        assert_ne!(
            signing_payload(&a),
            signing_payload(&b),
            "template change must produce different signing payload"
        );
    }

    #[test]
    fn decode_rejects_non_v3() {
        // v=1 (old schema — no template field): CBOR decode succeeds but version check fails.
        // We can only encode a v1 token if it has a template field (same struct), but
        // the decode gate is on the version number, so encode v=1 with the new struct
        // and verify we get a re-issue error.
        let t = InviteToken {
            v: 1,
            issuer_did: "did:key:a".into(),
            peer_id: "p".into(),
            ticket: "/ip4/1".into(),
            profiles: vec![],
            network_id: "default".into(),
            issued_at: "t".into(),
            template: "conversation".into(),
            sig: vec![],
        };
        let enc = encode(&t).unwrap();
        let err = decode(&enc).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "expected re-issue/newer in error, got: {err}"
        );
    }

    #[test]
    fn decode_rejects_v2() {
        // v2 tokens (no template field) must be rejected so the issuer re-mints.
        let t = InviteToken {
            v: 2,
            issuer_did: "did:key:a".into(),
            peer_id: "p".into(),
            ticket: "/ip4/1".into(),
            profiles: vec!["chat-requests".into()],
            network_id: "default".into(),
            issued_at: "t".into(),
            template: "conversation".into(), // present in struct but token is v=2
            sig: vec![],
        };
        let enc = encode(&t).unwrap();
        let err = decode(&enc).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "expected re-issue/newer in error for v2, got: {err}"
        );
    }

    #[test]
    fn decode_rejects_wrong_prefix() {
        let err = decode("wrong-prefix").unwrap_err().to_string();
        assert!(err.contains("invalid pairing invite token prefix"));
    }

    #[test]
    fn decode_rejects_truncated_base58() {
        let enc = encode(&sample_token(3)).unwrap();
        let truncated = &enc[..enc.len() - 4];
        let err = decode(truncated).unwrap_err().to_string();
        assert!(
            err.contains("decoding pairing invite token")
                || err.contains("parsing pairing invite token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn signing_payload_is_stable_across_sig_changes() {
        let mut a = sample_token(3);
        a.sig = vec![0xAA; 64];
        let mut b = sample_token(3);
        b.sig = vec![0xBB; 64];
        assert_eq!(signing_payload(&a), signing_payload(&b));
    }
}
