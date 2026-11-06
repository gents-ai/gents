//! Pairing templates remain the owner of directional DID/requester predicates.

use std::collections::BTreeSet;

use anyhow::{bail, Result};

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
        let profile = P2pCollectionProfile::from_id(profile_id)
            .ok_or_else(|| anyhow::anyhow!("unknown P2P collection profile {profile_id:?}"))?;
        if profile.collection_names().is_empty() {
            bail!("P2P profile {profile_id:?} contains runtime-local data; use authenticated GraphQL access");
        }
        for collection in profile.collection_names() {
            collections.insert((*collection).to_string());
        }
    }

    if collections.is_empty() {
        bail!("provide at least one --collection or --profile");
    }

    for collection in &collections {
        if !gents_protocol::schemas::is_replicated_protocol_collection(collection) {
            bail!("collection {collection:?} is runtime-local; use authenticated GraphQL access");
        }
    }
    Ok(collections)
}

const RUNTIME_COLLECTIONS: &[&str] = &[];
const AGENT_COLLECTIONS: &[&str] = &[];
const DESKTOP_CONFIG_COLLECTIONS: &[&str] = &[];
const CHAT_REQUEST_COLLECTIONS: &[&str] = &[];
const TOOL_SERVICE_COLLECTIONS: &[&str] = &[];
const DISCOVERY_COLLECTIONS: &[&str] = &["PeerRegistry", "NodeDirectoryEntry"];
