use defra_node::{EmbeddedVeraPolicy, PolicyNonce};
use gents_protocol::schemas::BuiltInPolicy;

/// The DID every home's embedded Vera deployment seeds the built-in
/// policies under, the creator vera.rs gives genesis policies. No key
/// answers for it, so no actor can edit or retire a seeded policy, and every
/// deployment derives the same ID for each built-in policy.
pub const VERA_POLICY_CREATOR: &str = vera_modules::acp::GENESIS_POLICY_CREATOR;

/// The nonce `policy` is seeded under: its position in
/// [`BuiltInPolicy::ALL`], fixed, so its ID is fixed with it.
fn nonce(policy: BuiltInPolicy) -> PolicyNonce {
    let position = BuiltInPolicy::ALL
        .iter()
        .position(|listed| *listed == policy)
        .expect("a built-in policy is listed");
    let mut nonce = PolicyNonce::default();
    if let Some(last) = nonce.0.last_mut() {
        *last = u8::try_from(position + 1).expect("few policies");
    }
    nonce
}

/// `policy` as an embedded Vera deployment is seeded with it.
pub fn seed(policy: &BuiltInPolicy) -> EmbeddedVeraPolicy {
    EmbeddedVeraPolicy {
        definition: policy.definition().to_owned(),
        nonce: nonce(*policy),
        creator: VERA_POLICY_CREATOR.to_owned(),
    }
}

pub fn seed_transport_v2(policy: &BuiltInPolicy) -> EmbeddedVeraPolicy {
    let mut seed = seed(policy);
    if *policy == BuiltInPolicy::Household {
        seed.definition = gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_TRANSPORT_V2.to_owned();
    }
    seed
}

#[cfg(test)]
mod tests {
    use gents_protocol::schemas::BuiltInPolicy;

    use crate::local::document_acp::{DocumentAcp, home_policy_ids};

    /// The IDs a Vera home names its built-in policies by are the IDs vera.rs
    /// derives for the policies its deployment is seeded with.
    #[test]
    fn a_vera_homes_ids_are_its_seeded_policies() {
        let ids = home_policy_ids(DocumentAcp::Vera);
        for policy in BuiltInPolicy::ALL {
            assert_eq!(
                super::seed(&policy)
                    .id()
                    .expect("a built-in policy compiles"),
                ids.id(policy),
                "{policy:?}"
            );
        }
    }

    #[test]
    fn v2_genesis_ids_match_its_seeds() {
        let ids = gents_protocol::schemas::HomePolicyIds::embedded_vera_transport_v2();
        for policy in BuiltInPolicy::ALL {
            assert_eq!(
                super::seed_transport_v2(&policy).id().unwrap(),
                ids.id(policy)
            );
        }
    }
}
