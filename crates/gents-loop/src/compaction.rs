use crate::claude_messages_body::ReplayCheckpointError;
use crate::loop_stream::ReplayEvidenceViolation;
use crate::provider_input::ProviderInputError;

pub const DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS: usize = 32_768;
/// Guided resamples a Claude summary chunk gets before the strict JSON
/// fallback.
pub const MAX_CLAUDE_GUIDED_SUMMARY_RETRIES: u32 = 1;

pub const MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS: usize = 32_768;
pub const DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX: usize = 100;
pub const MAX_COMPACTION_SUMMARY_FILE_LIST_MAX: usize = 1_000;

#[cfg(test)]
#[path = "compaction_checkpoint_tests.rs"]
mod checkpoint_tests;
#[path = "compaction_engine.rs"]
mod engine;
#[path = "compaction_history.rs"]
pub mod history;
#[path = "compaction_reduction.rs"]
mod reduction;
#[path = "compaction_rolling.rs"]
mod rolling;
#[path = "compaction_summary.rs"]
pub mod summary;
#[path = "compaction_view.rs"]
mod view;

pub use engine::{ProviderReductionEngine, ReductionEngine};
pub use reduction::{
    apply_reduction_decision, reduction_decision, rolling_plan_is_valid, rolling_step_input,
    AssociationReduction, CompactionStrategy, ExactReduction, FileActivity, ReductionAdmission,
    ReductionDecision, ReductionMode, ReductionOptions, ReductionOutcome, ReductionResult,
};
pub use view::{
    association_view_with_sources, compacted_through_sequence, pair_safe_boundary, provider_view,
    provider_view_with_sources, safe_to_reduce, sanitize_association_with_sources,
    sanitize_history_for_provider, session_cursor_for_reduction, session_prefix_is_stable,
    split_for_summary, strip_tool_results,
};

#[derive(Debug, thiserror::Error)]
pub enum ReductionError {
    #[error("compaction_deadline_elapsed_before_summary_dispatch")]
    DeadlineElapsed,
    #[error(
        "compaction_indivisible_unit_cannot_fit: start={start}, end={end}, \
         estimated_input_tokens={estimated_input_tokens}, input_budget={input_budget}, \
         reserved_output_tokens={reserved_output_tokens}, context_window={context_window}"
    )]
    IndivisibleUnitCannotFit {
        start: usize,
        end: usize,
        estimated_input_tokens: usize,
        input_budget: usize,
        reserved_output_tokens: usize,
        context_window: usize,
    },
    #[error("compaction_exact_count_overflow: field={field}, value={value}")]
    ExactCountOverflow { field: &'static str, value: usize },
    #[error("compaction_invalid_rolling_plan")]
    InvalidRollingPlan,
    #[error("compaction_cannot_fit_provider_visible_history: the conversation cannot be reduced enough for this model's context budget without removing required history. Start a new session or use a larger context window.")]
    CannotFit,
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("exact reduction is not the provider order of its association rows")]
    AssociationOrder,
    #[error("projecting compactable provider messages")]
    ProjectCompactable(#[source] ProviderInputError),
    #[error("projecting original compaction input")]
    ProjectOriginal(#[source] ProviderInputError),
    #[error("projecting normalized compaction input")]
    ProjectNormalized(#[source] ProviderInputError),
    #[error("projecting final compacted provider input")]
    ProjectFinal(#[source] ProviderInputError),
    #[error("session compaction prefix exceeds the canonical provider history")]
    PrefixExceedsHistory,
    #[error("exact reduction does not partition the active session provider view")]
    ExactPartition,
    #[error("projecting rolling compaction summary request")]
    ProjectSummaryRequest(#[source] ProviderInputError),
    #[error("exact reduction split exceeds its association rows")]
    SplitExceedsRows,
    #[error("session compaction provider-prefix count overflow")]
    PrefixOverflow,
    #[error(transparent)]
    Reduction(#[from] ReductionError),
    #[error(transparent)]
    EstimateMessages(#[from] ProviderInputError),
    #[error(transparent)]
    BuildSummaryRequest(#[from] crate::error::LoopStreamError),
    #[error(transparent)]
    ReplayCheckpoint(#[from] ReplayCheckpointError),
    #[error(transparent)]
    ReplayEvidence(#[from] ReplayEvidenceViolation),
    #[error("compaction_provider_failure: {deadline_context}{diagnostic}")]
    GuidedProviderFailure {
        deadline_context: &'static str,
        diagnostic: String,
    },
    #[error("compaction_provider_failure: guided structured output failed: {guided}; non-guided JSON fallback failed: {fallback}")]
    FallbackProviderFailure { guided: String, fallback: String },
    /// The provider rejected a summary request as over its context window.
    #[error("compaction_provider_failure: summary request exceeded the provider's context window: {diagnostic}")]
    SummaryContextOverflow {
        overflow: crate::error::ContextOverflow,
        diagnostic: String,
    },
    #[error("guided compaction exhausted the request token budget")]
    GuidedBudgetExhausted(#[source] crate::loop_stream::OneShotError),
    #[error("non-guided compaction fallback exhausted the request token budget")]
    FallbackBudgetExhausted(#[source] crate::loop_stream::OneShotError),
}

impl CompactionError {
    pub fn aggregate_token_budget_exhaustion(&self) -> Option<&str> {
        match self {
            Self::BuildSummaryRequest(error) => error.aggregate_token_budget_exhaustion(),
            Self::GuidedBudgetExhausted(error) | Self::FallbackBudgetExhausted(error) => {
                error.aggregate_token_budget_exhaustion()
            }
            _ => None,
        }
    }
}

type Result<T, E = CompactionError> = std::result::Result<T, E>;

pub fn bounded_summary(summary: String) -> String {
    let (bounded, _, _) = crate::truncation::truncate_text(
        &summary,
        crate::truncation::TruncationMode::Head,
        &crate::truncation::TruncationLimits::default(),
    );
    bounded
}

pub fn merge_optional_params(
    left: Option<serde_json::Value>,
    right: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (left, right) {
        (Some(left), Some(right)) => Some(merge_json_values(left, right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn merge_json_values(left: serde_json::Value, right: serde_json::Value) -> serde_json::Value {
    match (left, right) {
        (serde_json::Value::Object(mut left), serde_json::Value::Object(right)) => {
            for (key, right_value) in right {
                let value = left
                    .remove(&key)
                    .map(|left_value| merge_json_values(left_value, right_value.clone()))
                    .unwrap_or(right_value);
                left.insert(key, value);
            }
            serde_json::Value::Object(left)
        }
        (_, right) => right,
    }
}
