//! Signed pairing-invite token (current version: v5).
//!
//! The token carries everything the joining peer needs to connect plus a
//! signature over that payload by the issuer's DID key.  The join command
//! verifies the signature (TOFU) before writing a `PeerPairingDesired` row.
//!
//! Encoding: CBOR (ciborium) → base58 → `"dapair1-"` prefix.
//!
//! Signing payload: CBOR of a copy of the token with `sig = []`.  This means
//! the signature covers `v`, `issuer_did`, `peer_id`, `ticket`, `nonce`,
//! `network_id`, `issued_at`, `template`, `grant`, and `network` — every field
//! that matters for correctness — while remaining stable across sig values.
//!
//! Version history:
//!   v2 — original release (profiles, no template)
//!   v3 — adds `template: String`; older tokens rejected with a re-issue hint
//!   v4 — drops the now-dead `profiles` field (scope comes solely from
//!        `template`) and adds a single-use `nonce: String`; older tokens
//!        rejected with a re-issue hint
//!   v5 — carries admin-signed `NetworkRecord` + `MembershipRecord` so join
//!        admission can be membership-gated before control-plane replication

use std::io::Cursor;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::network_token::{MembershipRecord, NetworkRecord};

/// Versioned pairing-invite envelope.  CBOR-encoded, bs58-encoded, prefixed.
///
/// Current version: v5.  The `grant` and `network` records were added in v5;
/// v4 and earlier tokens are rejected on decode with a re-issue hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteToken {
    pub v: u8,
    pub issuer_did: String,
    pub peer_id: String,
    pub ticket: String,
    /// Single-use invite nonce. Generated fresh at mint; the join path records it
    /// in a consumed-nonce ledger and rejects any token whose nonce was already
    /// redeemed (the runtime ledger lands in Task C2). Mirrors the Lean
    /// `Token.nonce` modeled in `Proofs/PeerRegistryDiscovery`.
    pub nonce: String,
    pub network_id: String,
    pub issued_at: String,
    /// Scope template id (e.g. `"conversation"`) selected by the invite issuer.
    /// Added in v3; the `join` command writes this as the desired row's template
    /// and (since v4) is the sole source of the pairing's collection scope.
    pub template: String,
    /// Admin-signed active grant for the intended joiner.
    pub grant: MembershipRecord,
    /// Admin-signed network root record for the grant.
    pub network: NetworkRecord,
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
/// token whose `v` field is not `5`.  Older tokens are rejected so the issuer
/// must re-mint with a current `defra-agent` binary.
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
        5 => Ok(token),
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

/// Default maximum age of a pairing invite before it is rejected at join.
///
/// `issued_at` is part of the signed payload, so this bounds the replay window of
/// a leaked or intercepted token without any token-format change. It is a coarse
/// freshness gate, NOT a single-use guarantee — a token can still be replayed
/// within the window; true single-use needs a server-tracked nonce (deferred).
pub const DEFAULT_INVITE_MAX_AGE: Duration = Duration::hours(1);

/// Verify that a token's signed `issued_at` is fresh relative to `now`: not older
/// than `max_age`, and not more than `max_age` in the future (clock-skew bound).
/// A malformed `issued_at` is rejected. This is the replay-window check the join
/// path runs after verifying the signature.
pub fn check_freshness(token: &InviteToken, now: DateTime<Utc>, max_age: Duration) -> Result<()> {
    check_issued_at_freshness(&token.issued_at, now, max_age)
}

/// Freshness check over a bare `issued_at` string, shared by the DID-bound
/// invite ([`check_freshness`]) and the bearer invite
/// (`bearer_token::check_bearer_freshness`).
pub fn check_issued_at_freshness(
    raw_issued_at: &str,
    now: DateTime<Utc>,
    max_age: Duration,
) -> Result<()> {
    let issued_at = DateTime::parse_from_rfc3339(raw_issued_at.trim())
        .with_context(|| format!("pairing invite has an unparseable issued_at {raw_issued_at:?}"))?
        .with_timezone(&Utc);
    let age = now.signed_duration_since(issued_at);
    if age > max_age {
        anyhow::bail!(
            "pairing invite expired: issued {issued_at} is older than the {} max age (re-issue the invite)",
            humantime_max_age(max_age)
        );
    }
    if age < -max_age {
        anyhow::bail!(
            "pairing invite issued_at {issued_at} is too far in the future (clock skew or forged); rejected"
        );
    }
    Ok(())
}

fn humantime_max_age(max_age: Duration) -> String {
    let secs = max_age.num_seconds();
    if secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else if secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
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
            created_at: "2026-06-13T00:00:00Z".into(),
            sig: vec![4, 5, 6],
        }
    }

    fn sample_grant() -> MembershipRecord {
        MembershipRecord {
            network_id: "default".into(),
            member_did: "did:key:member".into(),
            status: "active".into(),
            granted_at: "2026-06-13T00:00:00Z".into(),
            revoked_at: String::new(),
            sig: vec![7, 8, 9],
        }
    }

    fn sample_token(v: u8) -> InviteToken {
        InviteToken {
            v,
            issuer_did: "did:key:a".into(),
            peer_id: "p".into(),
            ticket: "/ip4/1".into(),
            nonce: "nonce-a".into(),
            network_id: "default".into(),
            issued_at: "2026-06-13T00:00:00Z".into(),
            template: "conversation".into(),
            grant: sample_grant(),
            network: sample_network(),
            sig: vec![1, 2, 3],
        }
    }

    #[test]
    fn token_v5_round_trips_and_signing_payload_ignores_sig() {
        let t = InviteToken {
            v: 5,
            issuer_did: "did:key:a".into(),
            peer_id: "p".into(),
            ticket: "/ip4/1".into(),
            nonce: "nonce-a".into(),
            network_id: "default".into(),
            issued_at: "2026-06-13T00:00:00Z".into(),
            template: "conversation".into(),
            grant: sample_grant(),
            network: sample_network(),
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
    fn token_v5_signing_payload_covers_nonce() {
        // Two tokens identical except nonce → different signing payloads.
        let mut a = sample_token(5);
        a.nonce = "nonce-a".into();
        a.sig = vec![];
        let mut b = sample_token(5);
        b.nonce = "nonce-b".into();
        b.sig = vec![];
        assert_ne!(
            signing_payload(&a),
            signing_payload(&b),
            "nonce change must produce different signing payload"
        );
    }

    #[test]
    fn token_v5_signing_payload_covers_network_id() {
        // Two tokens identical except network_id → different signing payloads.
        let mut a = sample_token(5);
        a.network_id = "default".into();
        a.sig = vec![];
        let mut b = sample_token(5);
        b.network_id = "staging".into();
        b.sig = vec![];
        assert_ne!(
            signing_payload(&a),
            signing_payload(&b),
            "network_id change must produce different signing payload"
        );
    }

    #[test]
    fn check_freshness_accepts_recent_and_rejects_stale_or_future() {
        let mut t = sample_token(5);
        let now = DateTime::parse_from_rfc3339("2026-06-13T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let max_age = Duration::hours(1);

        // issued 30m ago — fresh.
        t.issued_at = "2026-06-13T00:30:00Z".into();
        assert!(check_freshness(&t, now, max_age).is_ok());

        // issued 2h ago — expired.
        t.issued_at = "2026-06-12T23:00:00Z".into();
        let err = check_freshness(&t, now, max_age).unwrap_err().to_string();
        assert!(err.contains("expired"), "unexpected error: {err}");

        // issued 2h in the future — rejected (skew/forgery).
        t.issued_at = "2026-06-13T03:00:00Z".into();
        let err = check_freshness(&t, now, max_age).unwrap_err().to_string();
        assert!(err.contains("future"), "unexpected error: {err}");

        // unparseable issued_at — rejected.
        t.issued_at = "not-a-timestamp".into();
        assert!(check_freshness(&t, now, max_age).is_err());
    }

    #[test]
    fn token_v5_signing_payload_covers_template() {
        // Two tokens identical except template → different signing payloads.
        let mut a = sample_token(5);
        a.template = "conversation".into();
        a.sig = vec![];
        let mut b = sample_token(5);
        b.template = "backup".into();
        b.sig = vec![];
        assert_ne!(
            signing_payload(&a),
            signing_payload(&b),
            "template change must produce different signing payload"
        );
    }

    #[test]
    fn token_v5_signing_payload_covers_grant_and_network() {
        let mut a = sample_token(5);
        let mut b = sample_token(5);
        b.grant.member_did = "did:key:other".into();
        assert_ne!(signing_payload(&a), signing_payload(&b));

        a = sample_token(5);
        b = sample_token(5);
        b.network.admin_did = "did:key:other-admin".into();
        assert_ne!(signing_payload(&a), signing_payload(&b));
    }

    #[test]
    fn decode_rejects_non_v5() {
        // v=1 (old schema): the decode gate is on the version number, so encode
        // v=1 with the current struct and verify we get a re-issue error.
        let mut t = sample_token(1);
        t.sig = vec![];
        let enc = encode(&t).unwrap();
        let err = decode(&enc).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "expected re-issue/newer in error, got: {err}"
        );
    }

    #[test]
    fn decode_rejects_v4() {
        // v4 tokens have no grant/network in the historic schema. This current
        // struct with v=4 still exercises the version gate: issuers must re-mint
        // with a current binary.
        let mut t = sample_token(4);
        t.sig = vec![];
        let enc = encode(&t).unwrap();
        let err = decode(&enc).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "expected re-issue/newer in error for v4, got: {err}"
        );
    }

    #[test]
    fn decode_rejects_wrong_prefix() {
        let err = decode("wrong-prefix").unwrap_err().to_string();
        assert!(err.contains("invalid pairing invite token prefix"));
    }

    #[test]
    fn decode_rejects_truncated_base58() {
        let enc = encode(&sample_token(5)).unwrap();
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
        let mut a = sample_token(5);
        a.sig = vec![0xAA; 64];
        let mut b = sample_token(5);
        b.sig = vec![0xBB; 64];
        assert_eq!(signing_payload(&a), signing_payload(&b));
    }
}
