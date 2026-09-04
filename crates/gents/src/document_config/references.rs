//! The id/model sets a config document's reference fields are checked
//! against (#1331). One loader, built from the current node state, feeds
//! every referential validator: `AgentBehavior::validate_references` is the
//! only consumer today, but the shape generalizes to any document that
//! points at a backend, tool selection, profile, or skill.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::backend_registry::list_backend_records;

use super::inference_profile::list_inference_profile_records;
use super::skill::list_skill_records;
use super::tool_selection::list_tool_selection_records;

/// Snapshot of the config documents a referencing document (today, only
/// `AgentBehavior`) may point at. Backends and profiles are global by
/// design (see `self_config::backend_request`/`profile_request`); tool
/// selections and skills are scoped to the referencing document's own
/// principal.
#[derive(Debug, Clone, Default)]
pub struct ConfigReferences {
    /// `backend_id` -> the models it advertises (empty means "any model is
    /// accepted", matching `InferenceBackend`'s own no-lockout semantics).
    pub backends: BTreeMap<String, Vec<String>>,
    pub tool_selections: BTreeSet<String>,
    pub profiles: BTreeSet<String>,
    pub skills: BTreeSet<String>,
}

impl ConfigReferences {
    /// Load the reference sets for `agent_did` from the current node state.
    pub async fn load(node: &EmbeddedNode, agent_did: &str) -> Result<Self> {
        let backends = list_backend_records(node)
            .await?
            .into_iter()
            .map(|(_, backend)| (backend.backend_id, backend.models))
            .collect();
        let tool_selections = list_tool_selection_records(node, agent_did)
            .await?
            .into_iter()
            .map(|(_, selection)| selection.selection_id)
            .collect();
        let profiles = list_inference_profile_records(node)
            .await?
            .into_iter()
            .map(|(_, profile)| profile.profile_id)
            .collect();
        let skills = list_skill_records(node, agent_did)
            .await?
            .into_iter()
            .map(|(_, skill)| skill.skill_id)
            .collect();
        Ok(Self {
            backends,
            tool_selections,
            profiles,
            skills,
        })
    }
}
