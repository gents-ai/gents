use std::collections::BTreeSet;

use gents_protocol::file_inventory::FileInventoryAdmission;
use serde::{Deserialize, Serialize};

/// Closed collection vocabulary of a session's canonical closure. Retired
/// response and spill documents cannot be selected. Origin dependencies retain
/// their ACP; following a reference never grants access to the whole origin
/// session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SessionCollection {
    AgentRequest,
    AgentMessage,
    AgentToolCall,
    AgentOutputSegment,
    CompactionEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDocumentKey {
    pub collection: SessionCollection,
    pub doc_id: String,
}

pub const SESSION_COLLECTIONS: &[SessionCollection] = &[
    SessionCollection::AgentRequest,
    SessionCollection::AgentMessage,
    SessionCollection::AgentToolCall,
    SessionCollection::AgentOutputSegment,
    SessionCollection::CompactionEntry,
];

pub fn session_collection_name(collection: SessionCollection) -> &'static str {
    match collection {
        SessionCollection::AgentRequest => "AgentRequest",
        SessionCollection::AgentMessage => "AgentMessage",
        SessionCollection::AgentToolCall => "AgentToolCall",
        SessionCollection::AgentOutputSegment => "AgentOutputSegment",
        SessionCollection::CompactionEntry => "CompactionEntry",
    }
}

pub const SESSION_OWNERSHIP_MISMATCH: &str = "session ownership does not match request";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionOwner {
    pub session_doc_id: Option<String>,
    pub session_id: String,
    pub requester_did: String,
    pub node_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionReadObservation {
    pub peer_id: String,
    pub requester_did: String,
    pub session_doc_id: String,
    pub source: SessionOwner,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppliedPairingRoute {
    pub peer_id: String,
    pub requester_did: String,
    pub node_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerifiedActiveMembership {
    pub network_id: String,
    pub member_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionDocument {
    pub collection: SessionCollection,
    pub doc_id: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InventoryCatalog {
    pub applied_pairing_routes: BTreeSet<AppliedPairingRoute>,
    pub selected_network_id: String,
    pub verified_active_memberships: BTreeSet<VerifiedActiveMembership>,
    pub sessions: BTreeSet<SessionOwner>,
    pub session_reads: BTreeSet<SessionReadObservation>,
    pub documents: BTreeSet<SessionDocument>,
    pub authorized_reference_closure: BTreeSet<SessionDocumentKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryVerdict {
    Admit(BTreeSet<SessionDocument>),
    Reject(&'static str),
}

/// Refines `FileInventory.admits` for an admission bound to its physical
/// session root.
pub fn decide_inventory_admission(
    admission: &FileInventoryAdmission,
    catalog: &InventoryCatalog,
) -> InventoryVerdict {
    let request = &admission.request;
    let physical_root = admission.session_doc_id.as_str();
    let pairing = AppliedPairingRoute {
        peer_id: request.peer_id.clone(),
        requester_did: request.requester_did.clone(),
        node_did: request.node_did.clone(),
    };
    if !catalog.applied_pairing_routes.contains(&pairing) {
        return InventoryVerdict::Reject("peer pairing does not match requester and agent");
    }
    let membership = VerifiedActiveMembership {
        network_id: catalog.selected_network_id.clone(),
        member_did: request.requester_did.clone(),
    };
    if !catalog.verified_active_memberships.contains(&membership) {
        return InventoryVerdict::Reject(
            "requester membership is not verified active in the selected network",
        );
    }
    let owner = catalog.sessions.iter().find(|source| {
        source.node_did == request.node_did
            && source.session_id == request.session_id
            && source.requester_did == request.requester_did
            && source.session_doc_id.as_deref() == Some(physical_root)
    });
    let source = if let Some(owner) = owner {
        owner.clone()
    } else {
        if physical_root.is_empty() {
            return InventoryVerdict::Reject(SESSION_OWNERSHIP_MISMATCH);
        }
        let mut observations = catalog.session_reads.iter().filter(|read| {
            read.peer_id == request.peer_id
                && read.requester_did == request.requester_did
                && read.session_doc_id == physical_root
                && read.source.node_did == request.node_did
                && read.source.session_id == request.session_id
                && read.source.session_doc_id.as_deref() == Some(physical_root)
        });
        let (Some(read), None) = (observations.next(), observations.next()) else {
            return InventoryVerdict::Reject(SESSION_OWNERSHIP_MISMATCH);
        };
        let mut roots = catalog.sessions.iter().filter(|source| {
            source.node_did == request.node_did && source.session_id == request.session_id
        });
        if !read.visible
            || read.source.requester_did.is_empty()
            || roots.next() != Some(&read.source)
            || roots.next().is_some()
        {
            return InventoryVerdict::Reject(SESSION_OWNERSHIP_MISMATCH);
        }
        read.source.clone()
    };

    InventoryVerdict::Admit(
        catalog
            .documents
            .iter()
            .filter(|doc| {
                SESSION_COLLECTIONS.contains(&doc.collection)
                    && doc.requester_did == source.requester_did
                    && doc.node_did == request.node_did
                    && (doc.session_id == request.session_id
                        || catalog.authorized_reference_closure.contains(
                            &SessionDocumentKey {
                                collection: doc.collection,
                                doc_id: doc.doc_id.clone(),
                            },
                        ))
            })
            .cloned()
            .collect(),
    )
}

