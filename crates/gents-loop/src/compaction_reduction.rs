use gents_protocol::message::Message;
use serde::{Deserialize, Serialize};

use super::{
    history, CompactionError, ReductionError, DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX,
    DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
};
use crate::provider_input::budget::{threshold_decision, ThresholdDecision};
use crate::provider_input::ProviderInputProfile;

pub fn rolling_plan_is_valid(
    target_messages: usize,
    chunk_messages: &[usize],
    chunk_pair_closed: &[bool],
    chunk_can_dispatch: &[bool],
    checkpoint_covered: usize,
) -> bool {
    !chunk_messages.is_empty()
        && chunk_messages.len() == chunk_pair_closed.len()
        && chunk_messages.len() == chunk_can_dispatch.len()
        && chunk_messages.iter().all(|messages| *messages > 0)
        && chunk_pair_closed.iter().all(|pair_closed| *pair_closed)
        && chunk_can_dispatch.iter().all(|can_dispatch| *can_dispatch)
        && chunk_messages
            .iter()
            .try_fold(0usize, |total, messages| total.checked_add(*messages))
            == Some(target_messages)
        && checkpoint_covered == target_messages
}

pub fn rolling_step_input<T>(prior_checkpoint: Option<T>, chunk: Vec<T>) -> Vec<T> {
    prior_checkpoint.into_iter().chain(chunk).collect()
}

#[derive(Debug, Clone)]
pub struct ReductionOptions {
    pub tool_result_max_chars: usize,
    pub keep_recent_tokens: usize,
    pub mode: ReductionMode,
    pub max_compacted_prefix_messages: Option<usize>,
    pub summary_max_output_tokens: usize,
    pub summary_file_list_max: usize,
    pub deadline: Option<chrono::DateTime<chrono::Utc>>,
    pub aggregate_token_budget: Option<crate::loop_stream::AggregateTokenBudget>,
    pub sampling_seed: Option<i64>,
    /// The requesting session's provider-input calibration for its own model.
    /// An engine whose summary requests go to that same model sizes its
    /// summary chunks with it; an engine summarizing with another model keeps
    /// its own calibration.
    pub context_calibration: Option<crate::provider_input::budget::ContextCalibration>,
}

impl Default for ReductionOptions {
    fn default() -> Self {
        Self {
            tool_result_max_chars: 2000,
            keep_recent_tokens: 20000,
            mode: ReductionMode::Summarize,
            max_compacted_prefix_messages: None,
            summary_max_output_tokens: DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
            summary_file_list_max: DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX,
            deadline: None,
            aggregate_token_budget: None,
            sampling_seed: None,
            context_calibration: None,
        }
    }
}

/// Why a reduction may run, in estimator units (the provider-input
/// estimator's raw count, before calibration). Both fields are in the same
/// units as `ProviderInputCounter::estimate_message_request`, so the
/// compactor can subtract and compare them with its own estimates; callers
/// convert provider-token budgets with `ContextCalibration::estimator_budget`
/// and anchored counts with `ContextCalibration::estimator_units`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReductionAdmission {
    input_tokens: usize,
    effective_input_budget: usize,
    threshold_decision: ThresholdDecision,
}

impl ReductionAdmission {
    /// The compaction trigger: admission when `context` exceeds
    /// `provider_budget` (provider tokens). The admission is in estimator
    /// units: the raw count, unless the anchor is what put the request over
    /// budget. Modelled by `PromptAssembly.ContextCalibration.State.admission`.
    pub fn for_context(
        context: crate::provider_input::budget::ContextEstimate,
        provider_budget: usize,
    ) -> Option<Self> {
        use crate::provider_input::budget;
        matches!(
            threshold_decision(context.tokens, provider_budget),
            ThresholdDecision::ReduceEligible
        )
        .then(|| {
            Self::required(
                if context.tokens > context.calibrated {
                    budget::estimator_input_units(context.tokens, context.ratio_basis_points)
                        .max(context.raw)
                } else {
                    context.raw
                },
                budget::estimator_input_budget(provider_budget, context.ratio_basis_points),
            )
        })
    }

    /// Admission when the caller has already decided reduction is required:
    /// the provider rejected the input as over its limit, or the anchored
    /// provider count is over budget where the raw estimate is not.
    pub fn required(input_tokens: usize, effective_input_budget: usize) -> Self {
        Self {
            input_tokens,
            effective_input_budget,
            threshold_decision: ThresholdDecision::ReduceEligible,
        }
    }

    pub(super) fn decision<C>(
        self,
        can_fit: bool,
        prefix_len: usize,
        checkpoint: C,
    ) -> ReductionDecision<C> {
        reduction_decision(self.threshold_decision, can_fit, prefix_len, checkpoint)
    }

    pub fn input_tokens(self) -> usize {
        self.input_tokens
    }

    pub fn effective_input_budget(self) -> usize {
        self.effective_input_budget
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReductionDecision<C> {
    NotNeeded,
    CannotFit,
    Reduce { prefix_len: usize, checkpoint: C },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReductionOutcome<T, C> {
    NotNeeded {
        messages: Vec<T>,
    },
    CannotFit,
    Reduced {
        compacted_prefix: Vec<T>,
        retained_suffix: Vec<T>,
        checkpoint: C,
    },
}

pub fn reduction_decision<C>(
    threshold: ThresholdDecision,
    can_fit: bool,
    prefix_len: usize,
    checkpoint: C,
) -> ReductionDecision<C> {
    match threshold {
        ThresholdDecision::NotNeeded => ReductionDecision::NotNeeded,
        ThresholdDecision::ReduceEligible if can_fit => ReductionDecision::Reduce {
            prefix_len,
            checkpoint,
        },
        ThresholdDecision::ReduceEligible => ReductionDecision::CannotFit,
    }
}

pub fn apply_reduction_decision<T, C>(
    mut source: Vec<T>,
    decision: ReductionDecision<C>,
) -> ReductionOutcome<T, C> {
    match decision {
        ReductionDecision::NotNeeded => ReductionOutcome::NotNeeded { messages: source },
        ReductionDecision::CannotFit => ReductionOutcome::CannotFit,
        ReductionDecision::Reduce {
            prefix_len,
            checkpoint,
        } if prefix_len > 0 && prefix_len <= source.len() => {
            let retained_suffix = source.split_off(prefix_len);
            ReductionOutcome::Reduced {
                compacted_prefix: source,
                retained_suffix,
                checkpoint,
            }
        }
        ReductionDecision::Reduce { .. } => ReductionOutcome::CannotFit,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum CompactionStrategy {
    StripToolResults,
    #[default]
    StripThenSummarize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReductionMode {
    StripOnly,
    Summarize,
}

impl CompactionStrategy {
    pub const ALL: [Self; 2] = [Self::StripToolResults, Self::StripThenSummarize];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::StripToolResults => "StripToolResults",
            Self::StripThenSummarize => "StripThenSummarize",
        }
    }

    pub const fn reduction_mode(&self) -> ReductionMode {
        match self {
            Self::StripToolResults => ReductionMode::StripOnly,
            Self::StripThenSummarize => ReductionMode::Summarize,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileActivity {
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
}

impl FileActivity {
    pub fn is_empty(&self) -> bool {
        self.files_read.is_empty() && self.files_modified.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct ReductionResult {
    pub(super) state: ReductionResultState,
    pub original_token_estimate: usize,
    pub compacted_token_estimate: usize,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) enum ReductionResultState {
    ProviderViewRepaired { messages: Vec<Message> },
    Decided(ReductionOutcome<Message, String>),
}

impl ReductionResult {
    pub fn provider_messages(&self) -> Result<&[Message], ReductionError> {
        match &self.state {
            ReductionResultState::ProviderViewRepaired { messages } => Ok(messages),
            ReductionResultState::Decided(outcome) => match outcome {
                ReductionOutcome::NotNeeded { messages } => Ok(messages),
                ReductionOutcome::Reduced {
                    retained_suffix, ..
                } => Ok(retained_suffix),
                ReductionOutcome::CannotFit => Err(ReductionError::CannotFit),
            },
        }
    }

    pub fn exact_reduction(&self) -> Option<ExactReduction<'_>> {
        match &self.state {
            ReductionResultState::Decided(ReductionOutcome::Reduced {
                compacted_prefix,
                retained_suffix,
                checkpoint,
            }) => Some(ExactReduction {
                compacted_prefix,
                retained_suffix,
                checkpoint,
            }),
            ReductionResultState::ProviderViewRepaired { .. }
            | ReductionResultState::Decided(
                ReductionOutcome::NotNeeded { .. } | ReductionOutcome::CannotFit,
            ) => None,
        }
    }

    pub fn cannot_fit(&self) -> bool {
        matches!(
            self.state,
            ReductionResultState::Decided(ReductionOutcome::CannotFit)
        )
    }
}

impl ReductionResultState {
    pub(super) fn decided(outcome: ReductionOutcome<Message, String>) -> Self {
        Self::Decided(outcome)
    }

    pub(super) fn provider_view_repaired(messages: Vec<Message>) -> Self {
        Self::ProviderViewRepaired { messages }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExactReduction<'a> {
    pub compacted_prefix: &'a [Message],
    pub retained_suffix: &'a [Message],
    pub checkpoint: &'a str,
}

impl ExactReduction<'_> {
    pub fn messages_compacted(&self) -> Result<u32, ReductionError> {
        u32::try_from(self.compacted_prefix.len()).map_err(|_| ReductionError::ExactCountOverflow {
            field: "messages_compacted",
            value: self.compacted_prefix.len(),
        })
    }

    pub fn checkpoint_messages(&self) -> Vec<Message> {
        let mut messages = crate::prompt::compaction_summary_message(std::slice::from_ref(
            &self.checkpoint.trim().to_string(),
        ))
        .into_iter()
        .collect::<Vec<_>>();
        messages.extend_from_slice(self.retained_suffix);
        messages
    }
}

#[derive(Debug, Clone)]
pub struct AssociationReduction {
    compacted_prefix: Vec<Message>,
    retained_suffix: Vec<Message>,
    checkpoint: String,
}

impl AssociationReduction {
    pub fn exact(&self) -> ExactReduction<'_> {
        ExactReduction {
            compacted_prefix: &self.compacted_prefix,
            retained_suffix: &self.retained_suffix,
            checkpoint: &self.checkpoint,
        }
    }
}

impl ExactReduction<'_> {
    pub fn over_association_view(
        &self,
        profile: ProviderInputProfile,
        compacted_prefix: Vec<Message>,
        retained_suffix: Vec<Message>,
    ) -> Result<AssociationReduction, CompactionError> {
        if history::normalize_assistant_content_order(profile, compacted_prefix.clone())
            != self.compacted_prefix
            || history::normalize_assistant_content_order(profile, retained_suffix.clone())
                != self.retained_suffix
        {
            return Err(CompactionError::AssociationOrder);
        }
        Ok(AssociationReduction {
            compacted_prefix,
            retained_suffix,
            checkpoint: self.checkpoint.to_string(),
        })
    }
}
