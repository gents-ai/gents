//! Pairing templates remain the owner of directional DID/requester predicates.

use std::collections::BTreeSet;

use anyhow::{bail, Result};

/// The only P2P collection profile: the signed discovery documents. Runtime
/// data is read through authenticated GraphQL, never replicated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum P2pCollectionProfile {
    Discovery,
}

impl P2pCollectionProfile {
    pub fn id(self) -> &'static str {
        match self {
            P2pCollectionProfile::Discovery => "discovery",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim() {
            "discovery" => Some(P2pCollectionProfile::Discovery),
            _ => None,
        }
    }

    pub fn collection_names(self) -> &'static [&'static str] {
        match self {
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

const DISCOVERY_COLLECTIONS: &[&str] = &["PeerRegistry", "NodeDirectoryEntry"];
