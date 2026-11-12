use std::error::Error;

use serde::{Deserialize, Serialize};

use super::agent::NextCall;
use crate::enrollment::AuthorizationLeaseState;

/// Lean `PeerOperations.peerOnline`.
pub fn peer_online(reports_online: bool, heartbeat_fresh: bool) -> bool {
    reports_online && heartbeat_fresh
}

/// Lean `PeerOperations.pairingReplicating`.
pub fn pairing_replicating(has_addresses: bool, all_addresses_applied: bool) -> bool {
    has_addresses && all_addresses_applied
}

/// Lean `PeerOperations.pairingSubscribed`.
pub fn pairing_subscribed(
    template_pushes: bool,
    replicating: bool,
    has_collections: bool,
    all_collections_applied: bool,
) -> bool {
    if template_pushes {
        replicating
    } else {
        has_collections && all_collections_applied
    }
}

/// Lean `PeerOperations.pairingConnected`.
pub fn pairing_connected(
    has_transport_peers: bool,
    transport_peer_connected: bool,
    own_peer_connected: bool,
) -> bool {
    if has_transport_peers {
        transport_peer_connected
    } else {
        own_peer_connected
    }
}

/// A registered peer with the answers every client shows: whether it is
/// online now and whether this node pairs with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredPeer {
    pub peer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub network_id: String,
    pub templates: Vec<String>,
    pub addresses: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub online: bool,
    pub paired: bool,
}

/// This node's own registry row, as the runtime reports the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRegistration {
    /// `None` when the runtime reported no P2P peer ID.
    pub peer_id: Option<String>,
    pub node_did: String,
    pub listen_addresses: Vec<String>,
    pub display_name: Option<String>,
    pub templates: Vec<String>,
    pub network_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryRegistered {
    pub peer_id: String,
    pub node_did: String,
    pub display_name: Option<String>,
    pub templates: Vec<String>,
    pub network_id: String,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryDeregistered {
    pub peer_id: String,
    pub removed_count: usize,
}

/// Lean `PeerOperations.RegistryRefusal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryRefusal {
    PeerIdUnknown,
    UnknownTemplate,
}

/// Lean `PeerOperations.RegistryRecovery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryRecovery {
    Status,
    ListTemplates,
}

impl RegistryRefusal {
    /// Lean `PeerOperations.RegistryRefusal.recovery`.
    pub fn recovery(self) -> RegistryRecovery {
        match self {
            Self::PeerIdUnknown => RegistryRecovery::Status,
            Self::UnknownTemplate => RegistryRecovery::ListTemplates,
        }
    }
}

impl RegistryRecovery {
    pub fn next_call(self) -> NextCall {
        match self {
            Self::Status => NextCall::Inspect { resource: "p2p" },
            Self::ListTemplates => NextCall::List {
                resource: "p2p-template",
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("the runtime reported no P2P peer ID; it is stopped or runs with P2P off")]
    PeerIdUnknown,
    #[error("template {0:?} is not one this node can offer")]
    UnknownTemplate(String),
    /// Another owner's failure (the store, ACP), unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl RegistryError {
    pub fn refusal(&self) -> Option<RegistryRefusal> {
        match self {
            Self::PeerIdUnknown => Some(RegistryRefusal::PeerIdUnknown),
            Self::UnknownTemplate(_) => Some(RegistryRefusal::UnknownTemplate),
            Self::Passthrough(_) => None,
        }
    }

    pub fn recovery(&self) -> NextCall {
        self.refusal()
            .map_or(RegistryRecovery::Status, RegistryRefusal::recovery)
            .next_call()
    }
}

/// Days left on an enrollment-owned route's signed authorization lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingLease {
    pub state: AuthorizationLeaseState,
    pub remaining_days: i64,
}

impl PairingLease {
    pub fn needs_renewal(&self) -> bool {
        self.state != AuthorizationLeaseState::Current
    }
}

/// A desired pairing with what the applied observation and the live
/// connections say about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pairing {
    pub peer_id: String,
    pub transport_peer_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(skip)]
    pub actor_did: Option<String>,
    pub collections: Vec<String>,
    pub replicator_addresses: Vec<String>,
    pub profiles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// The signed enrollment generation this route materializes; absent on a
    /// local overlay row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrollment_authorization_sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrollment_authorization_expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_lease: Option<PairingLease>,
    pub connected: bool,
    pub subscribed: bool,
    pub replicating: bool,
}

/// The pairing observations an operator may grant an Agent reader on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingReaderTargets {
    pub desired: Vec<PairingReaderTarget>,
    pub applied: Vec<PairingReaderTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingReaderTarget {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(default)]
    pub peer_id: Option<String>,
    #[serde(default)]
    pub node_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_did: Option<String>,
}
