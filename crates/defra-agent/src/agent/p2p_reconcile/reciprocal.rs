use std::collections::BTreeSet;

use super::network::NetworkEndpointEntry;

/// Select endpoints that can materialize a reciprocal conversation data-plane
/// edge for a previously invited member DID.
///
/// This is intentionally pure: the store layer verifies `PeerEndpoint` records
/// and supplies only signed endpoint entries; the derivation only joins those
/// entries with `ReciprocalConversationIntent.member_did` and defers entries
/// without a dialable peer id/address.
pub fn derive_reciprocal_desired<'a>(
    intent_dids: &BTreeSet<String>,
    endpoints: &'a [NetworkEndpointEntry],
) -> Vec<&'a NetworkEndpointEntry> {
    endpoints
        .iter()
        .filter(|entry| intent_dids.contains(&entry.agent_did))
        .filter(|entry| !entry.peer_id.trim().is_empty())
        .filter(|entry| !entry.address.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(did: &str, peer_id: &str, address: &str) -> NetworkEndpointEntry {
        NetworkEndpointEntry {
            peer_id: peer_id.to_string(),
            agent_did: did.to_string(),
            address: address.to_string(),
        }
    }

    #[test]
    fn derive_reciprocal_desired_selects_endpoint_for_intent_did() {
        let intents = BTreeSet::from(["did:key:phone".to_string()]);
        let endpoints = vec![endpoint("did:key:phone", "peer-phone", "/ticket/phone")];

        let desired = derive_reciprocal_desired(&intents, &endpoints);

        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].peer_id, "peer-phone");
        assert_eq!(desired[0].agent_did, "did:key:phone");
        assert_eq!(desired[0].address, "/ticket/phone");
    }

    #[test]
    fn derive_reciprocal_desired_defers_without_matching_endpoint() {
        let intents = BTreeSet::from(["did:key:phone".to_string()]);
        let endpoints = vec![endpoint("did:key:other", "peer-other", "/ticket/other")];

        let desired = derive_reciprocal_desired(&intents, &endpoints);

        assert!(desired.is_empty());
    }

    #[test]
    fn derive_reciprocal_desired_ignores_blank_peer_id_or_address() {
        let intents = BTreeSet::from(["did:key:phone".to_string()]);
        let endpoints = vec![
            endpoint("did:key:phone", "", "/ticket/phone"),
            endpoint("did:key:phone", "peer-phone", ""),
            endpoint("did:key:phone", "   ", "/ticket/phone"),
            endpoint("did:key:phone", "peer-phone", "   "),
        ];

        let desired = derive_reciprocal_desired(&intents, &endpoints);

        assert!(desired.is_empty());
    }
}
