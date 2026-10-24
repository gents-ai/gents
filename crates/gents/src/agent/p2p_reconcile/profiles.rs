//! Pairing templates remain the owner of directional DID/requester predicates.

use std::collections::BTreeSet;

use super::error::{P2pReconcileError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum P2pCollectionProfile {
    Runtime,
    Agent,
    DesktopConfig,
    ChatRequests,
    ToolServices,
    Discovery,
}

impl P2pCollectionProfile {
    pub fn id(self) -> &'static str {
        match self {
            P2pCollectionProfile::Runtime => "runtime",
            P2pCollectionProfile::Agent => "agent",
            P2pCollectionProfile::DesktopConfig => "desktop-config",
            P2pCollectionProfile::ChatRequests => "chat-requests",
            P2pCollectionProfile::ToolServices => "tool-services",
            P2pCollectionProfile::Discovery => "discovery",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim() {
            "runtime" => Some(P2pCollectionProfile::Runtime),
            "agent" => Some(P2pCollectionProfile::Agent),
            "desktop-config" => Some(P2pCollectionProfile::DesktopConfig),
            "chat-requests" => Some(P2pCollectionProfile::ChatRequests),
            "tool-services" => Some(P2pCollectionProfile::ToolServices),
            "discovery" => Some(P2pCollectionProfile::Discovery),
            _ => None,
        }
    }

    pub fn collection_names(self) -> &'static [&'static str] {
        match self {
            P2pCollectionProfile::Runtime => RUNTIME_COLLECTIONS,
            P2pCollectionProfile::Agent => AGENT_COLLECTIONS,
            P2pCollectionProfile::DesktopConfig => DESKTOP_CONFIG_COLLECTIONS,
            P2pCollectionProfile::ChatRequests => CHAT_REQUEST_COLLECTIONS,
            P2pCollectionProfile::ToolServices => TOOL_SERVICE_COLLECTIONS,
            P2pCollectionProfile::Discovery => DISCOVERY_COLLECTIONS,
        }
    }
}

pub fn expand_p2p_collection_profile_ids<'a>(
    explicit_collections: impl IntoIterator<Item = &'a str>,
    profile_ids: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeSet<String>> {
    let mut collections = explicit_collections
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();

    for profile_id in profile_ids {
        let profile_id = profile_id.trim();
        if profile_id.is_empty() {
            continue;
        }
        let profile = P2pCollectionProfile::from_id(profile_id).ok_or_else(|| {
            P2pReconcileError::UnknownCollectionProfile {
                id: profile_id.to_owned(),
            }
        })?;
        for collection in profile.collection_names() {
            collections.insert((*collection).to_string());
        }
    }

    if collections.is_empty() {
        return Err(P2pReconcileError::NoCollectionsOrProfiles);
    }

    Ok(collections)
}

const RUNTIME_COLLECTIONS: &[&str] = &[
    "Node",
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
    "ChainKeyBinding",
    "EthTool",
    "InferenceBackend",
    "NodeRuntime",
    "NodeReadiness",
    "AgentRequest",
    "AgentSessionInputEdit",
    "AgentSessionGoalControl",
    "GoalObservation",
    "AgentOutputSegment",
    "AgentSession",
    "AgentMessage",
    "AgentToolCall",
    "CompactionEntry",
    "ProjectionAcpBinding",
    "Task",
    "Schedule",
    "Trigger",
    "EventSource",
];

const AGENT_COLLECTIONS: &[&str] = &[
    "Node",
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
    "ChainKeyBinding",
    "EthTool",
    "InferenceBackend",
    "NodeRuntime",
    "NodeReadiness",
];

const DESKTOP_CONFIG_COLLECTIONS: &[&str] = &[
    "Node",
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
    "ChainKeyBinding",
    "EthTool",
    "Task",
    "Schedule",
    "Trigger",
    "EventSource",
];

const CHAT_REQUEST_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentSessionInputEdit",
    "AgentSessionGoalControl",
    "GoalObservation",
    "AgentOutputSegment",
    "AgentSession",
    "AgentMessage",
    "AgentToolCall",
    "CompactionEntry",
];

const TOOL_SERVICE_COLLECTIONS: &[&str] = &["ToolServiceRegistry"];

const DISCOVERY_COLLECTIONS: &[&str] = &[
    "PeerRegistry",
    "Node",
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
    "ChainKeyBinding",
    "EthTool",
    "NodeRuntime",
    "NodeReadiness",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_select_canonical_documents_without_implicit_client_credentials() {
        for profile in [
            P2pCollectionProfile::Runtime,
            P2pCollectionProfile::Agent,
            P2pCollectionProfile::DesktopConfig,
            P2pCollectionProfile::ChatRequests,
            P2pCollectionProfile::ToolServices,
            P2pCollectionProfile::Discovery,
        ] {
            let names = profile.collection_names();
            assert_eq!(
                names.len(),
                names.iter().collect::<BTreeSet<_>>().len(),
                "duplicate profile member: {}",
                profile.id()
            );
            for name in names {
                assert!(
                    gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(name)
                        || gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.contains(name),
                    "{} selects retired/unregistered {name}",
                    profile.id()
                );
            }
        }
        for profile in [
            P2pCollectionProfile::DesktopConfig,
            P2pCollectionProfile::Discovery,
            P2pCollectionProfile::ChatRequests,
        ] {
            for secret in gents_protocol::schemas::CREDENTIAL_COLLECTION_NAMES {
                assert!(!profile.collection_names().contains(secret));
            }
        }
        for profile in [
            P2pCollectionProfile::Runtime,
            P2pCollectionProfile::ChatRequests,
        ] {
            assert!(
                profile
                    .collection_names()
                    .contains(&"AgentSessionInputEdit"),
                "{} must carry signed pending-input edits and their receipts",
                profile.id()
            );
        }
        let operator = super::super::templates::resolve_template("agent-config").unwrap();
        for profile in [P2pCollectionProfile::Runtime, P2pCollectionProfile::Agent] {
            for name in operator.collections {
                assert!(
                    profile.collection_names().contains(name),
                    "{} loses operator config {name}",
                    profile.id()
                );
            }
        }
    }
}
