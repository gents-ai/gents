use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::inference::compaction::CompactionStrategy;

/// Unset limits retain the existing runtime defaults. These are desired limits:
/// provider budgets and pair-safe transcript splitting can further constrain them.
/// Checkpoint instructions/schema remain runtime-owned. Request deadlines,
/// aggregate token accounting, seeds, and generated checkpoints are execution state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct CompactionConfig {
    pub compaction_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<CompactionStrategy>", optional = nullable))]
    pub strategy: CompactionStrategy,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub keep_recent_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub tool_result_max_chars: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub summary_max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub summary_file_list_max: Option<i64>,
    // Applies only to summarizing strategies.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub inference_profile_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl CompactionConfig {
    // Compaction-owned validation, the single owner every write path calls;
    // reference existence stays with the caller's reference snapshot.
    pub fn validation_violations(&self) -> Vec<String> {
        let compaction_id = self.compaction_id.trim();
        let mut violations: Vec<String> = Vec::new();

        if self
            .threshold
            .is_some_and(|value| !value.is_finite() || value <= 0.0 || value > 1.0)
        {
            violations.push(format!(
                "CompactionConfig {compaction_id} threshold must be within (0, 1]"
            ));
        }
        for (name, value) in [
            ("keep_recent_tokens", self.keep_recent_tokens),
            ("tool_result_max_chars", self.tool_result_max_chars),
            ("summary_max_output_tokens", self.summary_max_output_tokens),
            ("summary_file_list_max", self.summary_file_list_max),
        ] {
            if value.is_some_and(|value| value <= 0) {
                violations.push(format!(
                    "CompactionConfig {compaction_id} {name} must be positive"
                ));
            }
        }

        violations
    }

    pub fn validate(&self) -> Result<()> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(violations.join("; "))
        }
    }
}
