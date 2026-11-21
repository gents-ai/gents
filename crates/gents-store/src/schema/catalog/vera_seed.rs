use defra_node::{EmbeddedVeraPolicy, Node as EmbeddedNode, PolicyNonce};

use super::{node_context, NodePolicy, SchemaError, NODE_POLICIES};

/// The DID every home's embedded Vera deployment seeds the built-in
/// policies under, the creator vera.rs gives genesis policies. No key
/// answers for it, so no actor can edit or retire a seeded policy, and every
/// deployment derives the same ID for each built-in policy.
pub const VERA_POLICY_CREATOR: &str = vera_modules::acp::GENESIS_POLICY_CREATOR;

/// The nonce `policy` is seeded under: its position in [`NODE_POLICIES`],
/// fixed, so its ID is fixed with it.
fn nonce(policy: &NodePolicy) -> PolicyNonce {
    let position = NODE_POLICIES
        .iter()
        .position(|listed| listed.definition == policy.definition)
        .expect("a built-in policy is listed");
    let mut nonce = PolicyNonce::default();
    if let Some(last) = nonce.0.last_mut() {
        *last = u8::try_from(position + 1).expect("few policies");
    }
    nonce
}

/// `policy` as an embedded Vera deployment is seeded with it.
pub fn seed(policy: &NodePolicy) -> EmbeddedVeraPolicy {
    EmbeddedVeraPolicy {
        definition: policy.definition.to_owned(),
        nonce: nonce(policy),
        creator: VERA_POLICY_CREATOR.to_owned(),
    }
}

/// The ID of `policy` on `node`'s embedded Vera deployment, which holds it
/// from its first block; `None` on a node whose document access is not
/// Vera's. The node adopts the seeded policy: its own create would make the
/// policy under its own DID instead of the seed's creator.
pub(super) async fn seeded_id(
    node: &EmbeddedNode,
    policy: &NodePolicy,
) -> Result<Option<String>, SchemaError> {
    if node.vera_acp().is_none() {
        return Ok(None);
    }
    let id = seed(policy).id().map_err(|_| (policy.mismatch)())?;
    match node.get_dac_policy_as(&node_context(node), &id).await? {
        Some(held) if held.id == id => Ok(Some(id)),
        _ => Err((policy.mismatch)()),
    }
}
