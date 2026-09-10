use anyhow::Result;
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct CompactionConfig {
    /// Logical configuration key; `_docID` is the storage identity.
    pub compaction_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<CompactionStrategy>", optional = nullable))]
    pub strategy: CompactionStrategy,
    /// Fraction of the context window used as the provider-input threshold.
    /// Unset uses the runtime default 0.75 (DEFAULT_COMPACTION_THRESHOLD).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub threshold: Option<f64>,
    /// Recent transcript retention target, not a guaranteed suffix size.
    /// Unset uses the current 20,000-token target; tool-call/result pairs stay intact.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub keep_recent_tokens: Option<i64>,
    /// Tool-result truncation limit when preparing material for summarization.
    /// Unset uses the current 2,000-character limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub tool_result_max_chars: Option<i64>,
    /// Summary completion output ceiling, independent of the user-turn output limit.
    /// Unset uses DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS (32,768), still
    /// bounded by the summary model's context and the shared request budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub summary_max_output_tokens: Option<i64>,
    /// Maximum file paths per list in the formatted checkpoint.
    /// Unset uses DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX (100).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub summary_file_list_max: Option<i64>,
    /// Optional inference profile for summary completions. Unset reuses the
    /// behavior's inference profile. Applies only to summarizing strategies;
    /// it cannot replace the enclosing request's deadline or token ledger.
    /// A separate profile is a new capability to wire after the contract update.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub inference_profile_id: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl CompactionConfig {
    /// Compaction-owned validation, the single owner every write path calls.
    /// Threshold must be within (0, 1] when set; token/char ceilings and the
    /// file-list maximum must be positive. Reference existence against the
    /// summary profile stays with the caller's reference snapshot.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compaction_rejects_nonfinite_thresholds_and_nonpositive_limits() {
        let mut config: CompactionConfig =
            serde_json::from_value(json!({"agent_did":"owner","compaction_id":"compact"})).unwrap();
        assert!(config.validate().is_ok());
        for threshold in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -0.1, 1.01] {
            config.threshold = Some(threshold);
            assert!(config.validate().is_err(), "{threshold}");
        }
        for threshold in [f64::MIN_POSITIVE, 0.75, 1.0] {
            config.threshold = Some(threshold);
            assert!(config.validate().is_ok());
        }
        config.keep_recent_tokens = Some(0);
        config.tool_result_max_chars = Some(-1);
        config.summary_max_output_tokens = Some(0);
        config.summary_file_list_max = Some(0);
        assert_eq!(config.validation_violations().len(), 4);
    }
}
