//! Authoritative client dataplane policy.
//!
//! Platform adapters own dialing and persistence. This module owns what a
//! route means: directory identity versus transport identity, direction,
//! collection membership, and filters. Keeping that vocabulary here prevents
//! bearer, desktop, mobile, CLI, and runtime repair paths from rebuilding
//! subtly different replicators.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use p2p::iroh::parse_public_peer_addr;

use super::templates::{
    combine_filters, equality_filter, scope_filter, PairingFilters, Scope, ScopeTemplate,
    CLIENT_COLLECTIONS, CLIENT_TO_RUNTIME_COLLECTIONS,
};
use super::{PairingApplied, PairingDesired};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingDirection {
    ClientToRuntime,
    RuntimeToClient,
}

pub const CLIENT_TO_RUNTIME_SUFFIX: &str = "client-to-runtime";
pub const RUNTIME_TO_CLIENT_SUFFIX: &str = "runtime-to-client";

pub fn client_route_id(directory_id: &str, direction: PairingDirection) -> String {
    format!("{directory_id}:{}", direction.as_str())
}

impl PairingDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClientToRuntime => "client-to-runtime",
            Self::RuntimeToClient => "runtime-to-client",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            CLIENT_TO_RUNTIME_SUFFIX => Ok(Self::ClientToRuntime),
            RUNTIME_TO_CLIENT_SUFFIX => Ok(Self::RuntimeToClient),
            other => bail!("unknown pairing direction {other:?}"),
        }
    }
}

/// Read the direction from a durable client-route key. Unsuffixed keys are
/// accepted as the legacy one-way client-to-runtime shape.
pub fn client_route_direction(pairing_id: &str) -> Result<Option<PairingDirection>> {
    let pairing_id = pairing_id.trim();
    if pairing_id.is_empty() {
        bail!("client pairing id must not be blank");
    }
    for (suffix, direction) in [
        (CLIENT_TO_RUNTIME_SUFFIX, PairingDirection::ClientToRuntime),
        (RUNTIME_TO_CLIENT_SUFFIX, PairingDirection::RuntimeToClient),
    ] {
        if let Some(directory_id) = pairing_id.strip_suffix(&format!(":{suffix}")) {
            if directory_id.trim().is_empty() {
                bail!("client pairing id has a blank directory identity");
            }
            return Ok(Some(direction));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportEndpoint {
    peer_id: String,
    address: String,
    dial_addresses: BTreeSet<String>,
}

impl TransportEndpoint {
    pub fn parse(address: impl Into<String>) -> Result<Self> {
        let address = address.into();
        let address = address.trim().to_string();
        let (peer_id, dial_addresses) = parse_public_peer_addr(&address)
            .context("pairing address is not a dialable Iroh endpoint")?;
        let peer_id = peer_id.to_string();
        if peer_id.len() != 64 || !peer_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("pairing address does not contain a valid Iroh transport peer id");
        }
        let dial_addresses = dial_addresses
            .into_iter()
            .map(|address| address.as_str().trim().to_string())
            .filter(|address| !address.is_empty())
            .collect::<BTreeSet<_>>();
        if dial_addresses.is_empty() {
            bail!("pairing address has no dialable Iroh transport address");
        }
        Ok(Self {
            peer_id,
            address,
            dial_addresses,
        })
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn equivalent_to(&self, other: &Self) -> bool {
        self.peer_id == other.peer_id && self.dial_addresses == other.dial_addresses
    }

    pub fn dial_address_count(&self) -> usize {
        self.dial_addresses.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRouteIdentity {
    pub directory_id: String,
    pub transport: TransportEndpoint,
    pub requester_did: String,
    pub owner_agent_did: String,
}

impl ClientRouteIdentity {
    pub fn new(
        directory_id: impl Into<String>,
        address: impl Into<String>,
        requester_did: impl Into<String>,
        owner_agent_did: impl Into<String>,
    ) -> Result<Self> {
        let directory_id = directory_id.into();
        let requester_did = requester_did.into();
        let owner_agent_did = owner_agent_did.into();
        if directory_id.trim().is_empty()
            || requester_did.trim().is_empty()
            || owner_agent_did.trim().is_empty()
        {
            bail!("client route directory id, requester DID, and owner DID must not be blank");
        }
        Ok(Self {
            directory_id,
            transport: TransportEndpoint::parse(address)?,
            requester_did,
            owner_agent_did,
        })
    }

    pub fn desired(&self, direction: PairingDirection) -> PairingDesired {
        let template = super::templates::resolve_template(super::templates::CLIENT_TEMPLATE)
            .expect("client route template is built in");
        let replicator_collections = client_route_collections(direction)
            .iter()
            .map(|collection| (*collection).to_string())
            .collect();
        PairingDesired {
            collections: Default::default(),
            replicator_addresses: [self.transport.address().to_string()].into_iter().collect(),
            replicator_collections,
            replicator_filter: resolve_template_filters(
                template,
                direction,
                &self.requester_did,
                &self.owner_agent_did,
            ),
            template_ids: [template.id.to_string()].into_iter().collect(),
        }
    }

    pub fn desired_id(&self, direction: PairingDirection) -> String {
        client_route_id(&self.directory_id, direction)
    }
}

pub fn client_route_collections(direction: PairingDirection) -> &'static [&'static str] {
    match direction {
        PairingDirection::ClientToRuntime => CLIENT_TO_RUNTIME_COLLECTIONS,
        PairingDirection::RuntimeToClient => CLIENT_COLLECTIONS,
    }
}

/// Resolve one template's filters for an explicitly named direction.
pub fn resolve_template_filters(
    template: &ScopeTemplate,
    direction: PairingDirection,
    requester_did: &str,
    owner_agent_did: &str,
) -> PairingFilters {
    match template.scope {
        Scope::ClientRoute => client_route_filters(direction, requester_did, owner_agent_did),
        _ => scope_filter(
            &template.scope,
            template.collections,
            requester_did,
            owner_agent_did,
        ),
    }
}

pub fn desired_route_is_applied(desired: &PairingDesired, applied: &PairingApplied) -> bool {
    !desired.replicator_addresses.is_empty()
        && desired.replicator_addresses == applied.replicator_addresses
        && desired.replicator_filter == applied.replicator_filter
        && applied.collections.is_subset(&desired.collections)
}

fn client_route_filters(
    direction: PairingDirection,
    requester_did: &str,
    owner_agent_did: &str,
) -> PairingFilters {
    let mut filters = PairingFilters::new();
    for collection in [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
    ] {
        filters.insert(
            collection.to_string(),
            combine_filters(
                equality_filter("requester_did", requester_did),
                equality_filter("agent_did", owner_agent_did),
            ),
        );
    }
    filters.insert(
        "BearerPairingReady".to_string(),
        combine_filters(
            equality_filter("claimant_did", requester_did),
            equality_filter("issuer_did", owner_agent_did),
        ),
    );
    let endpoint_did = match direction {
        PairingDirection::ClientToRuntime => requester_did,
        PairingDirection::RuntimeToClient => owner_agent_did,
    };
    filters.insert(
        "PeerEndpoint".to_string(),
        equality_filter("did", endpoint_did),
    );
    // Runtime-owned configuration is unfiltered only on the return leg. Each
    // replicator has one runtime source, while its mutable owner fields cannot
    // legally participate in DefraDB replication filters.
    debug_assert!(filters
        .keys()
        .all(|name| CLIENT_COLLECTIONS.contains(&name.as_str())));
    filters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::p2p_reconcile::templates::{
        filter_conditions, resolve_template, CLIENT_TEMPLATE,
    };

    #[test]
    fn directory_identity_is_not_accepted_as_a_transport_endpoint() {
        let error = TransportEndpoint::parse("0a7675bb-3378-4b66-ae33-a1490f7aa9f9")
            .expect_err("directory UUID is not dialable");
        assert!(error.to_string().contains("Iroh transport peer id"));
    }

    #[test]
    fn dialable_endpoint_exposes_transport_peer_separately() {
        let peer_id = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
        let endpoint = TransportEndpoint::parse(format!("127.0.0.1:56000/p2p/{peer_id}"))
            .expect("valid Iroh endpoint");
        assert_eq!(endpoint.peer_id(), peer_id);
        assert_ne!(endpoint.peer_id(), "directory-row-uuid");
    }

    #[test]
    fn route_key_direction_is_explicit_and_legacy_keys_remain_outbound() {
        assert_eq!(
            client_route_direction("directory-a:client-to-runtime").unwrap(),
            Some(PairingDirection::ClientToRuntime)
        );
        assert_eq!(
            client_route_direction("directory-a:runtime-to-client").unwrap(),
            Some(PairingDirection::RuntimeToClient)
        );
        assert_eq!(client_route_direction("legacy-peer").unwrap(), None);
        assert_eq!(
            client_route_direction("did:key:phone:legacy-route").unwrap(),
            None
        );
    }

    #[test]
    fn identity_only_iroh_endpoint_is_not_dialable() {
        let peer_id = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
        let error = TransportEndpoint::parse(format!("iroh://{peer_id}"))
            .expect_err("identity-only endpoint must not be accepted");
        assert!(error.to_string().contains("no dialable"));
    }

    #[test]
    fn client_route_conjoins_requester_and_destination_agent() {
        let template = resolve_template(CLIENT_TEMPLATE).unwrap();
        let filters = resolve_template_filters(
            template,
            PairingDirection::ClientToRuntime,
            "did:key:phone",
            "did:key:mandrake",
        );
        let conditions = filter_conditions(filters.get("AgentRequest").unwrap()).unwrap();
        let encoded = serde_json::to_string(&conditions).unwrap();
        assert!(encoded.contains("requester_did"));
        assert!(encoded.contains("did:key:phone"));
        assert!(encoded.contains("agent_did"));
        assert!(encoded.contains("did:key:mandrake"));
        assert!(!encoded.contains("did:key:amy"));
    }

    #[test]
    fn client_route_contains_exact_bounded_control_plane() {
        let template = resolve_template(CLIENT_TEMPLATE).unwrap();
        for collection in [
            "AgentBehavior",
            "ToolSelection",
            "InferenceProfile",
            "ToolServiceRegistry",
            "Skill",
            "DatastoreToolSurface",
            "Task",
            "Schedule",
            "EventTrigger",
        ] {
            assert!(
                template.collections.contains(&collection),
                "missing {collection}"
            );
        }
        assert!(!template.collections.contains(&"PeerPairingDesired"));
        assert!(!template.collections.contains(&"DataPlanePairingDesired"));
        assert!(
            !client_route_collections(PairingDirection::ClientToRuntime).contains(&"AgentBehavior")
        );
        assert!(
            client_route_collections(PairingDirection::RuntimeToClient).contains(&"AgentBehavior")
        );
        assert!(
            !client_route_collections(PairingDirection::RuntimeToClient)
                .contains(&"InferenceBackend"),
            "raw inference credentials must never replicate to clients"
        );
    }
}
