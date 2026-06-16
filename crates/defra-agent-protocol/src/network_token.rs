//! Canonical signing payloads for the network control-plane records, plus the
//! `danet1-` network-bootstrap pointer token.
//!
//! The four control-plane DefraDB collections (`AgentNetwork`,
//! `NetworkMembership`, `PeerEndpoint`, `NetworkJoinRequest`) each carry a
//! signature over their content. The records here are the *canonical signing
//! form* of those rows: the content fields plus a `sig` field that is zeroed
//! when computing the bytes to sign/verify. CLI, runtime, and tests construct
//! the same struct and therefore sign identical bytes — mirroring
//! [`crate::pairing_token::signing_payload`].
//!
//! The `danet1-` [`NetworkPointer`] is the network-bootstrap analogue of the
//! pairwise `dapair1-` invite: it identifies a network and how to reach its
//! admin, but carries **no membership grant** (membership is authored by the
//! admin as a `NetworkMembership` row). Encoding mirrors the invite token:
//! CBOR (ciborium) → base58 → `"danet1-"` prefix.

use std::io::Cursor;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Deterministic, admin-bound network id computed before signing.
///
/// `network_id` is itself a signed `AgentNetwork` field, so it cannot depend on
/// DefraDB's `_docID` or any other storage detail created after insertion.
pub fn derive_network_id(admin_did: &str, name: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut h = Sha256::new();
    h.update(admin_did.as_bytes());
    h.update(b"\x1f");
    h.update(name.as_bytes());
    let digest = h.finalize();
    format!("net-{}", bs58::encode(&digest[..16]).into_string())
}

/// Canonical signing form of an `AgentNetwork` row. `sig` is the admin DID's
/// signature over [`signing_payload`](NetworkRecord::signing_payload).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRecord {
    pub network_id: String,
    pub admin_did: String,
    pub display_name: String,
    pub default_template: String,
    pub created_at: String,
    /// Signature over the record with this field zeroed.
    pub sig: Vec<u8>,
}

/// Canonical signing form of a `NetworkMembership` row. `sig` is the admin DID's
/// signature over [`signing_payload`](MembershipRecord::signing_payload).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipRecord {
    pub network_id: String,
    pub member_did: String,
    pub status: String,
    pub granted_at: String,
    pub revoked_at: String,
    /// Signature over the record with this field zeroed.
    pub sig: Vec<u8>,
}

/// Canonical signing form of a `PeerEndpoint` row. `sig` is the member DID's
/// signature over [`signing_payload`](EndpointRecord::signing_payload).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRecord {
    pub did: String,
    pub node_id: String,
    pub address: String,
    pub updated_at: String,
    /// Signature over the record with this field zeroed.
    pub sig: Vec<u8>,
}

/// Canonical signing form of a `NetworkJoinRequest` row. `sig` is the candidate
/// DID's signature over [`signing_payload`](JoinRequestRecord::signing_payload).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequestRecord {
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_node_id: String,
    pub candidate_address: String,
    pub requested_at: String,
    /// Signature over the record with this field zeroed.
    pub sig: Vec<u8>,
}

macro_rules! signing_payload_impl {
    ($t:ty) => {
        impl $t {
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
    };
}
signing_payload_impl!(NetworkRecord);
signing_payload_impl!(MembershipRecord);
signing_payload_impl!(EndpointRecord);
signing_payload_impl!(JoinRequestRecord);

/// Network bootstrap pointer: identifies a network and how to reach its admin.
///
/// Distinct from the pairwise `dapair1-` invite — it carries no membership
/// grant. A joiner decodes this to learn the network's id, the admin DID to
/// trust (TOFU), and the admin's transport `ticket`, then sends a
/// `NetworkJoinRequest`; the admin authors the actual `NetworkMembership`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPointer {
    pub v: u8,
    pub network_id: String,
    pub admin_did: String,
    pub admin_ticket: String,
    pub issued_at: String,
    pub nonce: String,
    /// Admin DID's signature over [`signing_payload`](NetworkPointer::signing_payload).
    pub sig: Vec<u8>,
}

/// Prefix for all encoded network pointers.
pub const NETWORK_POINTER_PREFIX: &str = "danet1-";
/// Current network-pointer version.
pub const NETWORK_POINTER_VERSION: u8 = 1;

impl NetworkPointer {
    /// CBOR of this pointer with `sig` zeroed — the bytes signed/verified.
    /// Covers `v` (downgrade guard), `network_id`, `admin_did`, `admin_ticket`,
    /// `issued_at`, and `nonce`. Mirrors [`crate::pairing_token::signing_payload`].
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.sig = Vec::new();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&unsigned, &mut bytes)
            .expect("CBOR serialisation of signing payload is infallible");
        bytes
    }
}

/// Encode a pointer as `NETWORK_POINTER_PREFIX` + base58(CBOR(pointer)).
pub fn encode_pointer(p: &NetworkPointer) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(p, &mut bytes).context("encoding network pointer")?;
    Ok(format!(
        "{NETWORK_POINTER_PREFIX}{}",
        bs58::encode(bytes).into_string()
    ))
}

/// Decode a `NETWORK_POINTER_PREFIX`-prefixed pointer string.
///
/// Returns an error (mentioning "re-issue with a newer defra-agent") for any
/// pointer whose `v` field is not [`NETWORK_POINTER_VERSION`].
pub fn decode_pointer(raw: &str) -> Result<NetworkPointer> {
    let encoded = raw
        .trim()
        .strip_prefix(NETWORK_POINTER_PREFIX)
        .context("invalid network pointer prefix")?;
    let bytes = bs58::decode(encoded)
        .into_vec()
        .context("decoding network pointer")?;
    let p: NetworkPointer =
        ciborium::de::from_reader(Cursor::new(bytes)).context("parsing network pointer")?;
    match p.v {
        NETWORK_POINTER_VERSION => Ok(p),
        v => anyhow::bail!(
            "network pointer version {v} is not supported; \
             re-issue with a newer defra-agent"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pointer() -> NetworkPointer {
        NetworkPointer {
            v: NETWORK_POINTER_VERSION,
            network_id: "default".into(),
            admin_did: "did:key:admin".into(),
            admin_ticket: "/ip4/1".into(),
            issued_at: "2026-06-15T00:00:00Z".into(),
            nonce: "nonce-a".into(),
            sig: vec![1, 2, 3],
        }
    }

    #[test]
    fn network_id_is_deterministic_and_admin_bound() {
        let a = derive_network_id("did:key:zAdmin", "Fleet One");
        let b = derive_network_id("did:key:zAdmin", "Fleet One");
        let other_admin = derive_network_id("did:key:zOther", "Fleet One");
        let other_name = derive_network_id("did:key:zAdmin", "Fleet Two");

        assert_eq!(a, b, "deterministic");
        assert_ne!(a, other_admin, "admin-bound");
        assert_ne!(a, other_name, "name-bound");
        assert!(a.starts_with("net-"), "stable, recognizable prefix");
    }

    // --- Record signing payloads ----------------------------------------

    #[test]
    fn network_record_signing_payload_excludes_sig_and_covers_fields() {
        let mut a = NetworkRecord {
            network_id: "default".into(),
            admin_did: "did:key:admin".into(),
            display_name: "Default".into(),
            default_template: "discovery".into(),
            created_at: "2026-06-15T00:00:00Z".into(),
            sig: vec![1, 2, 3],
        };
        // sig is excluded: changing only sig leaves the payload unchanged.
        let mut sig_changed = a.clone();
        sig_changed.sig = vec![9, 9, 9];
        assert_eq!(a.signing_payload(), sig_changed.signing_payload());
        // a content field is covered.
        let baseline = a.signing_payload();
        a.network_id = "staging".into();
        assert_ne!(baseline, a.signing_payload());
    }

    #[test]
    fn membership_record_signing_payload_excludes_sig_and_covers_fields() {
        let mut a = MembershipRecord {
            network_id: "default".into(),
            member_did: "did:key:member".into(),
            status: "active".into(),
            granted_at: "2026-06-15T00:00:00Z".into(),
            revoked_at: String::new(),
            sig: vec![1, 2, 3],
        };
        let mut sig_changed = a.clone();
        sig_changed.sig = vec![9, 9, 9];
        assert_eq!(a.signing_payload(), sig_changed.signing_payload());
        let baseline = a.signing_payload();
        a.status = "revoked".into();
        assert_ne!(baseline, a.signing_payload());
    }

    #[test]
    fn endpoint_record_signing_payload_excludes_sig_and_covers_fields() {
        let mut a = EndpointRecord {
            did: "did:key:member".into(),
            node_id: "node-a".into(),
            address: "/ip4/1".into(),
            updated_at: "2026-06-15T00:00:00Z".into(),
            sig: vec![1, 2, 3],
        };
        let mut sig_changed = a.clone();
        sig_changed.sig = vec![9, 9, 9];
        assert_eq!(a.signing_payload(), sig_changed.signing_payload());
        let baseline = a.signing_payload();
        a.address = "/ip4/2".into();
        assert_ne!(baseline, a.signing_payload());
    }

    #[test]
    fn join_request_record_signing_payload_excludes_sig_and_covers_fields() {
        let mut a = JoinRequestRecord {
            network_id: "default".into(),
            candidate_did: "did:key:cand".into(),
            candidate_node_id: "node-c".into(),
            candidate_address: "/ip4/1".into(),
            requested_at: "2026-06-15T00:00:00Z".into(),
            sig: vec![1, 2, 3],
        };
        let mut sig_changed = a.clone();
        sig_changed.sig = vec![9, 9, 9];
        assert_eq!(a.signing_payload(), sig_changed.signing_payload());
        let baseline = a.signing_payload();
        a.candidate_did = "did:key:other".into();
        assert_ne!(baseline, a.signing_payload());
    }

    // --- danet1- network pointer ----------------------------------------

    #[test]
    fn pointer_round_trips_and_signing_payload_ignores_sig() {
        let p = sample_pointer();
        let enc = encode_pointer(&p).unwrap();
        assert!(enc.starts_with(NETWORK_POINTER_PREFIX));
        assert_eq!(decode_pointer(&enc).unwrap(), p);

        let mut p2 = p.clone();
        p2.sig = vec![9, 9, 9];
        assert_eq!(p.signing_payload(), p2.signing_payload());
    }

    #[test]
    fn pointer_signing_payload_covers_network_id_admin_did_and_ticket() {
        let baseline = sample_pointer().signing_payload();

        let mut a = sample_pointer();
        a.network_id = "staging".into();
        assert_ne!(baseline, a.signing_payload(), "network_id must be covered");

        let mut b = sample_pointer();
        b.admin_did = "did:key:other".into();
        assert_ne!(baseline, b.signing_payload(), "admin_did must be covered");

        let mut c = sample_pointer();
        c.admin_ticket = "/ip4/2".into();
        assert_ne!(
            baseline,
            c.signing_payload(),
            "admin_ticket must be covered"
        );
    }

    #[test]
    fn decode_pointer_rejects_wrong_prefix() {
        // A dapair1- invite (wrong prefix) must be rejected by the pointer decoder.
        let err = decode_pointer("dapair1-abc").unwrap_err().to_string();
        assert!(
            err.contains("invalid network pointer prefix"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn decode_pointer_rejects_wrong_version() {
        let mut p = sample_pointer();
        p.v = 2;
        let enc = encode_pointer(&p).unwrap();
        let err = decode_pointer(&enc).unwrap_err().to_string();
        assert!(
            err.contains("re-issue") || err.contains("newer"),
            "expected re-issue/newer in error, got: {err}"
        );
    }
}
