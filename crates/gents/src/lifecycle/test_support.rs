//! Shared fixture for the `#[1336]` pinning tests (`lifecycle::materialize`,
//! `lifecycle::queue`, `lifecycle::background_wake_recovery`,
//! `tool_call_lifecycle::subagent_request`): one fixed Ed25519 identity so
//! every pinning test signs with the same registered DID and therefore
//! produces the same signature bytes for the same payload.

use crate::identity::AgentIdentity;

/// Raw 64-byte (seed || public key) Ed25519 identity material, captured
/// once so every pinning test signs with the same registered DID.
pub(crate) const PIN_FIXED_KEY_HEX: &str = "4cbf8c1186d2fcb70559342fd142650a5ec5938d26a187d87e2c061b530d7be46edb79d5f548207182f7911b55709c9e4b9961c709486e5ce920e306470fe6d6";
pub(crate) const PIN_FIXED_DID: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";

pub(crate) fn pin_fixed_signing_identity(dir: &std::path::Path) -> crate::identity::KeyIdentity {
    let key_bytes: Vec<u8> = (0..PIN_FIXED_KEY_HEX.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&PIN_FIXED_KEY_HEX[offset..offset + 2], 16).unwrap())
        .collect();
    let path = dir.join("pinning.key");
    std::fs::write(&path, &key_bytes).expect("write fixed pinning key");
    let identity =
        crate::identity::KeyIdentity::load_or_create(&path, None).expect("load fixed identity");
    assert_eq!(
        identity.did(),
        PIN_FIXED_DID,
        "fixed pinning key derives a stable DID"
    );
    identity
}
