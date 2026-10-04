//! Persisted delegation targets selected by SubagentTools.target_ids.
//! The caller owns node_did; target_node_did owns the destination agent.
//! The model uses the friendly name. No JSON-in-string target configuration.

use serde::{Deserialize, Serialize};

/// Persisted delegation target selected by SubagentTools.target_ids.
/// The calling node owns this configuration; the destination node
/// remains explicit for local and cross-node delegation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct SubagentTargetDocument {
    pub target_id: String,
    /// Node that owns this target configuration (DefraDB ACP applies).
    pub node_did: String,
    /// Node that owns the destination agent.
    pub target_node_did: String,
    pub agent_id: String,
    /// Friendly callable name exposed to the model.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}
