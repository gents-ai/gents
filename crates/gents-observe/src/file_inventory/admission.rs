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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionDocument {
    pub collection: SessionCollection,
    pub doc_id: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}
