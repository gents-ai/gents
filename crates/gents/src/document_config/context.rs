use serde::{Deserialize, Serialize};

/// Reusable context configuration referenced by a behavior.
///
/// This document describes how context is assembled; conversation messages,
/// activated skills, and generated compaction entries remain session state.
/// References identify documents, not pinned revisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentContext {
    /// Logical configuration key; `_docID` is the storage identity.
    pub context_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Literal system instructions, used unchanged when assembling the preamble.
    /// No template evaluation; dynamic prompt templates belong to tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Absent uses default StripThenSummarize compaction with default limits.
    /// An explicit compaction document selects a different strategy or limits.
    pub compaction_id: Option<String>,
    /// Explicit skill allowlist. Empty means no skills; principal scope does not
    /// implicitly add skills. Selected skills must still belong to the principal
    /// and be enabled, and cannot grant tools beyond the context's tool selection.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skill_ids: Vec<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}
