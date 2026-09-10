use serde::{Deserialize, Serialize};

use crate::compaction::CompactionStrategy;

/// Compaction configuration referenced by a context.
///
/// Unset limits retain the existing runtime defaults. These are desired limits:
/// provider budgets and pair-safe transcript splitting can further constrain them.
/// Checkpoint instructions/schema remain runtime-owned. Request deadlines,
/// aggregate token accounting, seeds, and generated checkpoints are execution state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    /// Logical configuration key; `_docID` is the storage identity.
    pub compaction_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    pub strategy: CompactionStrategy,
    /// Fraction of the context window used as the provider-input threshold.
    /// Unset uses DEFAULT_COMPACTION_THRESHOLD (0.75).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// Recent transcript retention target, not a guaranteed suffix size.
    /// Unset uses the current 20,000-token target; tool-call/result pairs stay intact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<i64>,
    /// Tool-result truncation limit when preparing material for summarization.
    /// Unset uses the current 2,000-character limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result_max_chars: Option<i64>,
    /// Summary completion output ceiling, independent of the user-turn output limit.
    /// Unset uses DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS (32,768), still
    /// bounded by the summary model's context and the shared request budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_max_output_tokens: Option<i64>,
    /// Maximum file paths per list in the formatted checkpoint.
    /// Unset uses DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX (100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_file_list_max: Option<i64>,
    /// Optional inference profile for summary completions. Unset reuses the
    /// behavior's inference profile. Applies only to summarizing strategies;
    /// it cannot replace the enclosing request's deadline or token ledger.
    /// A separate profile is a new capability to wire after the contract update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_profile_id: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}
