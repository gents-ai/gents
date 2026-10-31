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

pub fn client_route_direction(pairing_id: &str) -> Result<PairingDirection> {
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
            return Ok(direction);
        }
    }
    bail!("client pairing id has no recognized direction suffix")
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
    pub owner_node_did: String,
}

impl ClientRouteIdentity {
    pub fn new(
        directory_id: impl Into<String>,
        address: impl Into<String>,
        requester_did: impl Into<String>,
        owner_node_did: impl Into<String>,
    ) -> Result<Self> {
        let directory_id = directory_id.into();
        let requester_did = requester_did.into();
        let owner_node_did = owner_node_did.into();
        if directory_id.trim().is_empty()
            || requester_did.trim().is_empty()
            || owner_node_did.trim().is_empty()
        {
            bail!("client route directory id, requester DID, and owner DID must not be blank");
        }
        Ok(Self {
            directory_id,
            transport: TransportEndpoint::parse(address)?,
            requester_did,
            owner_node_did,
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
                &self.owner_node_did,
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

pub fn resolve_template_filters(
    template: &ScopeTemplate,
    direction: PairingDirection,
    requester_did: &str,
    owner_node_did: &str,
) -> PairingFilters {
    match template.scope {
        Scope::ClientRoute => client_route_filters(direction, requester_did, owner_node_did),
        _ => scope_filter(
            &template.scope,
            template.collections,
            requester_did,
            owner_node_did,
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
    owner_node_did: &str,
) -> PairingFilters {
    let mut filters = PairingFilters::new();
    for collection in [
        "AgentRequest",
        "AgentMessage",
        "AgentToolCall",
        "AgentOutputSegment",
        "AgentSession",
        "CompactionEntry",
        "MailboxItem",
    ] {
        filters.insert(
            collection.to_string(),
            combine_filters(
                equality_filter("requester_did", requester_did),
                equality_filter("node_did", owner_node_did),
            ),
        );
    }
    let endpoint_did = match direction {
        PairingDirection::ClientToRuntime => requester_did,
        PairingDirection::RuntimeToClient => owner_node_did,
    };
    filters.insert(
        "PeerEndpoint".to_string(),
        equality_filter("did", endpoint_did),
    );
    filters.insert(
        "SessionHydrationRequest".to_string(),
        combine_filters(
            equality_filter("requester_did", requester_did),
            equality_filter("node_did", owner_node_did),
        ),
    );
    for collection in [
        "FileImageHydrationRequest",
        "FileInventoryImageRequest",
        "FileInventoryRequest",
        "AgentSessionInputEdit",
        "AgentSessionGoalControl",
    ] {
        filters.insert(
            collection.to_owned(),
            combine_filters(
                equality_filter("requester_did", requester_did),
                equality_filter("node_did", owner_node_did),
            ),
        );
    }
    if direction == PairingDirection::RuntimeToClient {
        filters.insert(
            "GoalObservation".to_owned(),
            combine_filters(
                equality_filter("requester_did", requester_did),
                equality_filter("node_did", owner_node_did),
            ),
        );
        filters.insert(
            "NodeReadiness".to_string(),
            equality_filter("node_did", owner_node_did),
        );
    }
    // Mutable owner fields cannot legally participate in DefraDB replication
    // filters, so runtime-owned configuration is unfiltered on the return leg.
    debug_assert!(filters
        .keys()
        .all(|name| CLIENT_COLLECTIONS.contains(&name.as_str())));
    filters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::p2p_reconcile::templates::{resolve_template, CLIENT_TEMPLATE};

    #[test]
    fn client_route_contains_exact_bounded_control_plane() {
        let template = resolve_template(CLIENT_TEMPLATE).unwrap();
        for collection in [
            "Agent",
            "AgentContext",
            "CompactionConfig",
            "Tools",
            "AgentTarget",
            "InferenceProfile",
            "InferenceSampling",
            "InferenceExecution",
            "InferenceRetryPolicy",
            "ToolServiceRegistry",
            "Skill",
            "DatastoreToolSurface",
            "Task",
            "Schedule",
            "Trigger",
            "EventSource",
        ] {
            assert!(
                template.collections.contains(&collection),
                "missing {collection}"
            );
        }
        assert!(!template.collections.contains(&"PeerPairingDesired"));
        assert!(!template.collections.contains(&"DataPlanePairingDesired"));
        assert!(!client_route_collections(PairingDirection::ClientToRuntime).contains(&"Agent"));
        assert!(client_route_collections(PairingDirection::RuntimeToClient).contains(&"Agent"));
        assert!(
            !client_route_collections(PairingDirection::RuntimeToClient)
                .contains(&"InferenceBackend"),
            "raw inference credentials must never replicate to clients"
        );
    }

    #[test]
    fn client_route_carries_session_hydration_requests() {
        let template = resolve_template(CLIENT_TEMPLATE).unwrap();
        let outbound = resolve_template_filters(
            template,
            PairingDirection::ClientToRuntime,
            "did:key:phone",
            "did:key:mandrake",
        );
        let returning = resolve_template_filters(
            template,
            PairingDirection::RuntimeToClient,
            "did:key:phone",
            "did:key:mandrake",
        );
        assert!(client_route_collections(PairingDirection::ClientToRuntime)
            .contains(&"SessionHydrationRequest"));
        assert!(template.collections.contains(&"SessionHydrationRequest"));
        let encoded = serde_json::to_string(outbound.get("SessionHydrationRequest").unwrap())
            .expect("encode hydration filter");
        assert!(encoded.contains("requester_did"));
        assert!(encoded.contains("did:key:phone"));
        assert!(encoded.contains("node_did"));
        assert!(encoded.contains("did:key:mandrake"));
        assert_eq!(
            outbound.get("SessionHydrationRequest"),
            returning.get("SessionHydrationRequest")
        );
    }
}
