use serde::{Deserialize, Serialize};

use super::references::ConfigReferences;
use super::DocumentConfigError;

/// This document describes how context is assembled; conversation messages,
/// activated skills, and generated compaction entries remain session state.
/// References identify documents, not pinned revisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AgentContext {
    pub context_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    // Literal system instructions, no template evaluation; dynamic prompt
    // templates belong to tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub system_prompt: Option<String>,
    // Absent grants no tools; there is no principal-wide implicit tool set.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub tools_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub compaction_id: Option<String>,
    // Explicit allowlist; empty means no skills, selected skills cannot grant
    // tools beyond the context's tool selection.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub skill_ids: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl AgentContext {
    pub fn reference_violations(&self, refs: &ConfigReferences) -> Vec<String> {
        self.validate_references(refs)
            .err()
            .map(|error| vec![error.to_string()])
            .unwrap_or_default()
    }

    pub fn validate_references(&self, refs: &ConfigReferences) -> Result<(), DocumentConfigError> {
        refs.validate_document(
            crate::Collection::AgentContext,
            &serde_json::to_value(self)?,
        )
    }
}
