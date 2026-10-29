use std::sync::Arc;

use crate::claude_messages_body::ReplayCheckpointError;
use crate::loop_stream::ReplayEvidenceViolation;
use crate::provider_input::ProviderInputError;
use crate::rig_compat::ProviderModel;
use futures::future::BoxFuture;
use gents_protocol::error_chain::ErrorChain;
use gents_protocol::message::Message;
use serde::{Deserialize, Serialize};

use crate::provider_input::ProviderInputProfile;

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
#[path = "compaction_history.rs"]
pub mod history;
#[path = "compaction_summary.rs"]
pub mod summary;

use history::{
    extract_file_activity, pretruncate_tool_results,
    split_messages_for_summary_with_counter_bounded,
};
use summary::{
    bounded_error_diagnostic, compaction_json_fallback_prompt, compaction_prompt,
    compaction_request_prompt, format_summary, parse_fallback_checkpoint, ContinuationCheckpoint,
};

use crate::provider_input::budget::{
    compaction_retention_target, configured_output_ceiling, rolling_summary_input_budget,
    summary_output_ceiling, threshold_decision, ThresholdDecision,
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

    fn decision<C>(self, can_fit: bool, prefix_len: usize, checkpoint: C) -> ReductionDecision<C> {
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
    state: ReductionResultState,
    pub original_token_estimate: usize,
    pub compacted_token_estimate: usize,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
}

#[derive(Debug, Clone)]
enum ReductionResultState {
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
    fn decided(outcome: ReductionOutcome<Message, String>) -> Self {
        Self::Decided(outcome)
    }

    fn provider_view_repaired(messages: Vec<Message>) -> Self {
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

type Result<T, E = CompactionError> = std::result::Result<T, E>;

pub trait ReductionEngine: Send + Sync {
    /// The retained tail's budget for reducing `messages` under `admission`
    /// with `options` against `context_window`, the same arguments `reduce`
    /// gets: the recovery band less the fixed layers and the checkpoint's
    /// reserve.
    fn retention_target(
        &self,
        configured_keep_recent: usize,
        messages: &[Message],
        admission: ReductionAdmission,
        options: &ReductionOptions,
        context_window: usize,
    ) -> Result<usize>;
    fn reduce<'a>(
        &'a self,
        messages: Vec<Message>,
        context_window: usize,
        options: &'a ReductionOptions,
        admission: ReductionAdmission,
    ) -> BoxFuture<'a, Result<ReductionResult>>;
}

#[derive(Clone)]
pub struct ProviderReductionEngine<M>
where
    M: ProviderModel,
{
    model: Arc<M>,
    config: crate::loop_stream::LoopConfig,
    source_input_counter: Option<Arc<crate::provider_input::ProviderInputCounter>>,
    summary_output_limit: Option<usize>,
    now: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
}

impl<M> ProviderReductionEngine<M>
where
    M: ProviderModel,
{
    pub fn new(model: Arc<M>, mut config: crate::loop_stream::LoopConfig) -> Self {
        config.retry_policy = crate::completion_retry::CompletionRetryPolicy::internal_immediate();
        Self {
            model,
            source_input_counter: None,
            summary_output_limit: None,
            config,
            now: Arc::new(chrono::Utc::now),
        }
    }

    pub fn with_summary_output_limit(mut self, limit: usize) -> Self {
        self.summary_output_limit = Some(limit);
        self
    }

    pub fn with_source_input_counter(
        mut self,
        counter: Arc<crate::provider_input::ProviderInputCounter>,
    ) -> Self {
        self.source_input_counter = Some(counter);
        self
    }

    /// The context window summary requests are sized against: this engine's
    /// own when it summarizes with another model, else the source's.
    fn summary_context_window(&self, source_context_window: usize) -> usize {
        if self.source_input_counter.is_some() {
            self.config.context_window
        } else {
            source_context_window
        }
    }

    /// The `max_tokens` each summary call of a reduction gets.
    fn summary_max_output_tokens(
        &self,
        options: &ReductionOptions,
        summary_context_window: usize,
    ) -> usize {
        summary_output_ceiling(
            options
                .summary_max_output_tokens
                .clamp(1, MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS)
                .min(self.summary_output_limit.unwrap_or(usize::MAX)),
            summary_context_window,
        )
    }

    /// The checkpoint's reserve is the summary call's output cap, in output
    /// tokens. Counted as estimator units it is conservative: replayed as
    /// input, a summary of that many provider tokens estimates at most that
    /// many (the ratio is at least one).
    pub fn retention_target(
        &self,
        configured_keep_recent: usize,
        compactable_messages: &[Message],
        admission: ReductionAdmission,
        options: &ReductionOptions,
        context_window: usize,
    ) -> Result<usize, CompactionError> {
        let compactable_input_tokens = self
            .source_input_counter
            .as_ref()
            .unwrap_or(&self.config.provider_input_counter)
            .estimate_message_request(compactable_messages)
            .map_err(CompactionError::ProjectCompactable)?;
        let fixed_input_tokens = admission
            .input_tokens()
            .saturating_sub(compactable_input_tokens);
        Ok(compaction_retention_target(
            configured_keep_recent,
            admission.effective_input_budget(),
            fixed_input_tokens,
            self.summary_max_output_tokens(options, self.summary_context_window(context_window)),
        ))
    }

    pub fn with_now(
        mut self,
        now: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
    ) -> Self {
        self.now = now;
        self
    }

    /// Whether this engine's summary requests are counted by the same
    /// estimator and model as the history it reduces.
    fn summarizes_with_source_model(&self) -> bool {
        self.source_input_counter.as_ref().is_none_or(|source| {
            source.profile() == self.config.provider_input_counter.profile()
                && source.model() == self.config.provider_input_counter.model()
        })
    }
}

impl<M> ReductionEngine for ProviderReductionEngine<M>
where
    M: ProviderModel,
{
    fn retention_target(
        &self,
        configured: usize,
        messages: &[Message],
        admission: ReductionAdmission,
        options: &ReductionOptions,
        context_window: usize,
    ) -> Result<usize> {
        ProviderReductionEngine::retention_target(
            self,
            configured,
            messages,
            admission,
            options,
            context_window,
        )
    }

    fn reduce<'a>(
        &'a self,
        messages: Vec<Message>,
        source_context_window: usize,
        options: &'a ReductionOptions,
        admission: ReductionAdmission,
    ) -> BoxFuture<'a, Result<ReductionResult>> {
        Box::pin(async move {
            let counter = self
                .source_input_counter
                .as_deref()
                .unwrap_or(self.config.provider_input_counter.as_ref());
            let context_window = self.summary_context_window(source_context_window);
            let original_token_estimate = counter
                .estimate_message_request(&messages)
                .map_err(CompactionError::ProjectOriginal)?;

            let (stripped_messages, stripped_activity) = provider_view(counter.profile(), messages);

            let stripped_token_estimate = counter
                .estimate_message_request(&stripped_messages)
                .map_err(CompactionError::ProjectNormalized)?;
            if matches!(options.mode, ReductionMode::StripOnly) {
                return Ok(ReductionResult {
                    state: ReductionResultState::provider_view_repaired(stripped_messages),
                    original_token_estimate,
                    compacted_token_estimate: stripped_token_estimate,
                    files_read: stripped_activity.files_read,
                    files_modified: stripped_activity.files_modified,
                });
            }

            let (old_messages, recent_messages) = split_messages_for_summary_with_counter_bounded(
                stripped_messages.clone(),
                options.keep_recent_tokens,
                counter,
                options.max_compacted_prefix_messages,
            )?;
            if old_messages.is_empty() {
                let state = if stripped_token_estimate < original_token_estimate {
                    ReductionResultState::provider_view_repaired(stripped_messages)
                } else {
                    ReductionResultState::decided(apply_reduction_decision(
                        stripped_messages,
                        admission.decision(false, 0, String::new()),
                    ))
                };
                return Ok(ReductionResult {
                    state,
                    original_token_estimate,
                    compacted_token_estimate: stripped_token_estimate,
                    files_read: stripped_activity.files_read,
                    files_modified: stripped_activity.files_modified,
                });
            }

            // Treat transcript messages as untrusted; instructions stay in the system layer.
            let mut summary_config = self.config.clone();
            summary_config.context_window = context_window;
            summary_config.preamble = Some(compaction_prompt().to_string());
            summary_config.context_message = None;
            summary_config.tool_choice = None;
            // The summarizer never compacts. A provider overflow of a summary
            // request ends this reduction instead of re-entering the
            // compactor, so the forced overflow reduction cannot recurse.
            summary_config.turn_compactor = None;
            // Summary chunks are sized against the provider's count: the
            // session's ratio when the summarizer is the session's model, else
            // this engine's own. Never an anchor: a summary chunk is not a
            // continuation of the anchored call, and the copy keeps summary
            // usage out of the session's and the engine's calibration.
            summary_config.context_calibration = options
                .context_calibration
                .as_ref()
                .filter(|_| self.summarizes_with_source_model())
                .unwrap_or(&self.config.context_calibration)
                .ratio_only();
            summary_config.replay = crate::loop_stream::LoopReplayInput::default();
            summary_config.max_turns = 0;
            let summary_max_output_tokens = self.summary_max_output_tokens(options, context_window);
            summary_config.max_tokens = Some(summary_max_output_tokens as u64);
            summary_config.aggregate_token_budget = options.aggregate_token_budget.clone();
            summary_config.additional_params = merge_optional_params(
                summary_config.additional_params.take(),
                options
                    .sampling_seed
                    .map(|seed| serde_json::json!({ "seed": seed })),
            );
            summary_config.deadline = match (options.deadline, summary_config.deadline) {
                (Some(from_options), Some(from_config)) => Some(from_options.min(from_config)),
                (from_options, from_config) => from_options.or(from_config),
            };
            let structured_output =
                crate::loop_stream::StructuredOutputConfig::for_type::<ContinuationCheckpoint>();
            // Claude decodes against the schema natively; each guided retry
            // resends the whole chunk, so one retry, then the fallback.
            let claude = self.config.provider_input_counter.profile()
                == crate::provider_input::ProviderInputProfile::ClaudeMessages;
            summary_config.structured_output = Some(if claude {
                structured_output.with_max_invalid_retries(MAX_CLAUDE_GUIDED_SUMMARY_RETRIES)
            } else {
                structured_output
            });
            if claude {
                // Every attempt on a chunk, guided or fallback, writes and
                // reads a cache entry at the end of the chunk history, before
                // the final instruction that differs between them.
                let mut params = summary_config
                    .additional_params
                    .take()
                    .filter(serde_json::Value::is_object)
                    .unwrap_or_else(|| serde_json::json!({}));
                params[crate::claude_messages_body::CACHE_PREFIX_BREAKPOINT_PARAM] =
                    serde_json::Value::Bool(true);
                summary_config.additional_params = Some(params);
            }

            let FileActivity {
                files_read,
                files_modified,
            } = extract_file_activity(&old_messages);
            let mut consumed = 0usize;
            let mut rolling_summary = None::<String>;
            let mut chunk_messages = Vec::new();
            let mut chunk_pair_closed = Vec::new();
            let mut chunk_can_dispatch = Vec::new();
            while consumed < old_messages.len() {
                // A prior chunk may have consumed the shared request deadline
                if deadline_elapsed(summary_config.deadline, self.now.as_ref()) {
                    return Err(CompactionError::Reduction(ReductionError::DeadlineElapsed));
                }
                let remaining = &old_messages[consumed..];
                let configured_output = configured_output_ceiling(summary_config.max_tokens);
                let mut max_chunk_messages = remaining.len();
                let (chunk_len, chunk_input_tokens, checkpoint) = loop {
                    let (chunk_len, chunk_input_tokens) = largest_fitting_summary_chunk(
                        self.model.as_ref(),
                        remaining,
                        max_chunk_messages,
                        rolling_summary.as_deref(),
                        &summary_config,
                        options,
                        consumed,
                        context_window,
                    )
                    .await?;
                    let chunk = pretruncate_tool_results(
                        remaining[..chunk_len].to_vec(),
                        options.tool_result_max_chars,
                    );
                    let prior_checkpoint = rolling_summary.as_ref().and_then(|summary| {
                        crate::prompt::compaction_summary_message(std::slice::from_ref(summary))
                    });
                    let prepared_history = rolling_step_input(prior_checkpoint, chunk);
                    match summarize_checkpoint(
                        self.model.as_ref(),
                        prepared_history,
                        summary_config.clone(),
                        self.now.clone(),
                    )
                    .await
                    {
                        Ok(checkpoint) => break (chunk_len, chunk_input_tokens, checkpoint),
                        // The provider counted the chunk over its limit even
                        // though the calibrated estimate fit: learn its count,
                        // then halve the chunk. Halving strictly shrinks the
                        // cap, so this ends once no pair-safe boundary fits.
                        Err(CompactionError::SummaryContextOverflow {
                            overflow,
                            diagnostic,
                        }) if history::pair_safe_boundaries(remaining)
                            .iter()
                            .any(|boundary| *boundary <= chunk_len / 2) =>
                        {
                            if let Some(observed) = overflow.input_tokens {
                                summary_config.context_calibration.observe(
                                    u64::from(observed),
                                    summary_config
                                        .context_calibration
                                        .estimator_units(chunk_input_tokens),
                                );
                            }
                            tracing::warn!(
                                chunk_start = consumed,
                                chunk_len,
                                chunk_input_tokens,
                                provider_input_tokens = overflow.input_tokens,
                                provider_limit = overflow.limit,
                                error = %diagnostic,
                                "summary chunk overflowed the provider's context window; halving it"
                            );
                            max_chunk_messages = chunk_len / 2;
                        }
                        Err(error) => return Err(error),
                    }
                };
                let pair_closed = history::pair_safe_boundaries(remaining).contains(&chunk_len);
                consumed = consumed
                    .checked_add(chunk_len)
                    .ok_or(CompactionError::Reduction(
                        ReductionError::ExactCountOverflow {
                            field: "messages_compacted",
                            value: old_messages.len(),
                        },
                    ))?;
                chunk_messages.push(chunk_len);
                chunk_pair_closed.push(pair_closed);
                chunk_can_dispatch.push(crate::provider_input::budget::can_dispatch(
                    chunk_input_tokens,
                    context_window,
                    configured_output,
                ));
                let (step_files_read, step_files_modified) = if consumed == old_messages.len() {
                    (&files_read[..], &files_modified[..])
                } else {
                    (&[][..], &[][..])
                };
                rolling_summary = Some(bounded_summary(format_summary(
                    &checkpoint,
                    step_files_read,
                    step_files_modified,
                    options
                        .summary_file_list_max
                        .clamp(1, MAX_COMPACTION_SUMMARY_FILE_LIST_MAX),
                )));
            }

            let summary = rolling_summary
                .expect("non-empty old prefix produces a checkpoint")
                .trim()
                .to_string();
            if summary.is_empty() {
                return Err(CompactionError::Reduction(
                    ReductionError::InvalidRollingPlan,
                ));
            }
            if !rolling_plan_is_valid(
                old_messages.len(),
                &chunk_messages,
                &chunk_pair_closed,
                &chunk_can_dispatch,
                consumed,
            ) {
                return Err(CompactionError::Reduction(
                    ReductionError::InvalidRollingPlan,
                ));
            }
            let mut compacted_projection =
                crate::prompt::compaction_summary_message(std::slice::from_ref(&summary))
                    .into_iter()
                    .collect::<Vec<_>>();
            compacted_projection.extend(recent_messages.iter().cloned());
            let compacted_token_estimate = counter
                .estimate_message_request(&compacted_projection)
                .map_err(CompactionError::ProjectFinal)?;
            let prefix_len = old_messages.len();
            u32::try_from(prefix_len).map_err(|_| {
                CompactionError::Reduction(ReductionError::ExactCountOverflow {
                    field: "messages_compacted",
                    value: prefix_len,
                })
            })?;
            let outcome = apply_reduction_decision(
                stripped_messages,
                admission.decision(true, prefix_len, summary),
            );
            if matches!(outcome, ReductionOutcome::CannotFit) {
                return Err(CompactionError::Reduction(
                    ReductionError::InvalidRollingPlan,
                ));
            }
            Ok(ReductionResult {
                state: ReductionResultState::decided(outcome),
                original_token_estimate,
                compacted_token_estimate,
                files_read,
                files_modified,
            })
        })
    }
}

/// The longest pair-safe prefix of `remaining`, at most `max_messages` long,
/// whose summary request fits the calibrated input budget.
#[allow(clippy::too_many_arguments)]
async fn largest_fitting_summary_chunk<M>(
    model: &M,
    remaining: &[Message],
    max_messages: usize,
    prior_summary: Option<&str>,
    summary_config: &crate::loop_stream::LoopConfig,
    options: &ReductionOptions,
    absolute_start: usize,
    context_window: usize,
) -> Result<(usize, usize), CompactionError>
where
    M: ProviderModel,
{
    let mut boundaries = history::pair_safe_boundaries(remaining);
    let all_boundaries = boundaries.len();
    boundaries.retain(|boundary| *boundary <= max_messages);
    if boundaries.is_empty() && all_boundaries > 0 {
        boundaries = history::pair_safe_boundaries(remaining);
        boundaries.truncate(1);
    }
    let configured_output = configured_output_ceiling(summary_config.max_tokens);
    let input_budget = rolling_summary_input_budget(context_window, configured_output);
    let Some(&first_boundary) = boundaries.first() else {
        let estimated_input_tokens =
            summary_candidate_tokens(model, remaining, prior_summary, summary_config, options)
                .await?;
        let absolute_end = absolute_start.checked_add(remaining.len()).ok_or(
            ReductionError::ExactCountOverflow {
                field: "indivisible_unit_end",
                value: absolute_start,
            },
        )?;
        return Err(ReductionError::IndivisibleUnitCannotFit {
            start: absolute_start,
            end: absolute_end,
            estimated_input_tokens,
            input_budget,
            reserved_output_tokens: configured_output,
            context_window,
        }
        .into());
    };

    let mut low = 0usize;
    let mut high = boundaries.len();
    let mut best = None;
    while low < high {
        let middle = low + (high - low) / 2;
        let boundary = boundaries[middle];
        let estimate = summary_candidate_tokens(
            model,
            &remaining[..boundary],
            prior_summary,
            summary_config,
            options,
        )
        .await?;
        if estimate <= input_budget
            && crate::provider_input::budget::can_dispatch(
                estimate,
                context_window,
                configured_output,
            )
        {
            best = Some((boundary, estimate));
            low = middle + 1;
        } else {
            high = middle;
        }
    }

    if let Some(best) = best {
        return Ok(best);
    }

    let estimated_input_tokens = summary_candidate_tokens(
        model,
        &remaining[..first_boundary],
        prior_summary,
        summary_config,
        options,
    )
    .await?;
    let absolute_end =
        absolute_start
            .checked_add(first_boundary)
            .ok_or(ReductionError::ExactCountOverflow {
                field: "indivisible_unit_end",
                value: absolute_start,
            })?;
    Err(ReductionError::IndivisibleUnitCannotFit {
        start: absolute_start,
        end: absolute_end,
        estimated_input_tokens,
        input_budget,
        reserved_output_tokens: configured_output,
        context_window,
    }
    .into())
}

async fn summary_candidate_tokens<M>(
    model: &M,
    chunk: &[Message],
    prior_summary: Option<&str>,
    summary_config: &crate::loop_stream::LoopConfig,
    options: &ReductionOptions,
) -> Result<usize, CompactionError>
where
    M: ProviderModel,
{
    let prepared_chunk = pretruncate_tool_results(chunk.to_vec(), options.tool_result_max_chars);
    let prior_checkpoint = prior_summary.and_then(|summary| {
        let summaries = [summary.to_string()];
        crate::prompt::compaction_summary_message(&summaries)
    });
    let history = rolling_step_input(prior_checkpoint, prepared_chunk);
    let request = crate::loop_stream::build_request(
        model,
        Message::user(compaction_request_prompt()),
        &history,
        &[],
        &[],
        summary_config,
    )
    .await
    .map_err(crate::rig_compat::loop_stream_error)?;
    summary_config
        .provider_input_counter
        .estimate_request(&request)
        .map(|estimate| summary_config.context_calibration.calibrated(estimate))
        .map_err(CompactionError::ProjectSummaryRequest)
}

async fn summarize_checkpoint<M>(
    model: &M,
    prepared_history: Vec<Message>,
    summary_config: crate::loop_stream::LoopConfig,
    now: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
) -> Result<ContinuationCheckpoint, CompactionError>
where
    M: ProviderModel,
{
    if deadline_elapsed(summary_config.deadline, now.as_ref()) {
        return Err(ReductionError::DeadlineElapsed.into());
    }
    let guided_result = crate::loop_stream::run_loop_to_typed(
        model.clone(),
        Message::user(compaction_request_prompt()),
        prepared_history.clone(),
        std::sync::Arc::new(Vec::new()),
        summary_config.clone(),
    )
    .await;
    match guided_result {
        Ok(checkpoint) => Ok(checkpoint),
        Err(guided_error)
            if is_guided_output_failure(&guided_error)
                && !deadline_elapsed(summary_config.deadline, now.as_ref()) =>
        {
            tracing::warn!(
                error = %bounded_error_diagnostic(&ErrorChain(&guided_error).to_string()),
                "guided compaction output exhausted recovery; trying one strict non-guided JSON fallback"
            );
            let mut fallback_config = summary_config;
            fallback_config.structured_output = None;
            fallback_config.on_rendered_request =
                Some(crate::rendered_request::scope::ambient_arming_sink(
                    crate::rendered_request::CaptureScopeKind::CompactionFallback,
                ));
            fallback_config.retry_policy =
                crate::completion_retry::CompletionRetryPolicy::no_retry();
            // The system prefix stays byte-identical to the guided attempts so
            // the provider's prompt cache still covers the chunk; the strict
            // JSON instruction rides in the final user message instead.
            let raw = crate::loop_stream::run_loop_to_text(
                model.clone(),
                Message::user(format!(
                    "{}\n\n{}",
                    compaction_request_prompt(),
                    compaction_json_fallback_prompt()
                )),
                prepared_history,
                std::sync::Arc::new(Vec::new()),
                fallback_config,
            )
            .await
            .map_err(|fallback_error| {
                if fallback_error.aggregate_token_budget_exhaustion().is_some() {
                    CompactionError::FallbackBudgetExhausted(fallback_error)
                } else {
                    CompactionError::FallbackProviderFailure {
                        guided: bounded_error_diagnostic(&ErrorChain(&guided_error).to_string()),
                        fallback: bounded_error_diagnostic(
                            &ErrorChain(&fallback_error).to_string(),
                        ),
                    }
                }
            })?;
            parse_fallback_checkpoint(&raw).map_err(|fallback_error| {
                CompactionError::FallbackProviderFailure {
                    guided: bounded_error_diagnostic(&ErrorChain(&guided_error).to_string()),
                    fallback: bounded_error_diagnostic(&fallback_error),
                }
            })
        }
        Err(error) => {
            if error.aggregate_token_budget_exhaustion().is_some() {
                return Err(CompactionError::GuidedBudgetExhausted(error));
            }
            if let Some(crate::error::InferenceError::ContextLengthExceeded {
                input_tokens,
                limit,
                ..
            }) = error
                .loop_stream_error()
                .map(crate::error::LoopStreamError::classify)
            {
                return Err(CompactionError::SummaryContextOverflow {
                    overflow: crate::error::ContextOverflow {
                        input_tokens,
                        limit,
                    },
                    diagnostic: bounded_error_diagnostic(&ErrorChain(&error).to_string()),
                });
            }
            let deadline_context = if deadline_elapsed(summary_config.deadline, now.as_ref()) {
                "compaction deadline elapsed after "
            } else {
                ""
            };
            Err(CompactionError::GuidedProviderFailure {
                deadline_context,
                diagnostic: bounded_error_diagnostic(&ErrorChain(&error).to_string()),
            })
        }
    }
}

fn is_guided_output_failure(error: &crate::loop_stream::OneShotError) -> bool {
    matches!(
        error
            .loop_stream_error()
            .and_then(crate::error::LoopStreamError::loop_verdict),
        Some((
            crate::error::LoopVerdict::StructuredOutputInvalid
                | crate::error::LoopVerdict::NoVisibleOutput,
            _
        ))
    )
}

fn deadline_elapsed(
    deadline: Option<chrono::DateTime<chrono::Utc>>,
    now: &(dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync),
) -> bool {
    deadline.is_some_and(|deadline| now() >= deadline)
}

pub fn strip_tool_results(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    history::strip_tool_results(messages)
}

pub fn sanitize_history_for_provider(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> Vec<Message> {
    history::normalize_assistant_content_order(
        profile,
        history::drop_unpaired_tool_calls(history::drop_orphaned_tool_results(messages)),
    )
}

pub fn provider_view(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> (Vec<Message>, FileActivity) {
    let (stripped, activity) = strip_tool_results(messages);
    (sanitize_history_for_provider(profile, stripped), activity)
}

pub fn provider_view_with_sources(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> (Vec<history::SourcedMessage>, FileActivity) {
    let sourced = history::source_messages(messages);
    let (stripped, activity) = history::strip_tool_results_sourced(sourced);
    let sanitized = sanitize_sourced(profile, stripped);
    (sanitized, activity)
}

pub fn association_view_with_sources(
    messages: Vec<Message>,
) -> (Vec<history::SourcedMessage>, FileActivity) {
    let sourced = history::source_messages(messages);
    let (stripped, activity) = history::strip_tool_results_sourced(sourced);
    (pair_sourced(stripped), activity)
}

pub fn sanitize_association_with_sources(messages: Vec<Message>) -> Vec<history::SourcedMessage> {
    pair_sourced(history::source_messages(messages))
}

fn pair_sourced(sourced: Vec<history::SourcedMessage>) -> Vec<history::SourcedMessage> {
    history::drop_unpaired_tool_calls_sourced(history::drop_orphaned_tool_results_sourced(sourced))
}

fn sanitize_sourced(
    profile: ProviderInputProfile,
    sourced: Vec<history::SourcedMessage>,
) -> Vec<history::SourcedMessage> {
    history::normalize_assistant_content_order_sourced(profile, pair_sourced(sourced))
}

pub fn compacted_through_sequence(
    profile: ProviderInputProfile,
    rows: &[(u32, Message)],
    provider_prefix_len: usize,
) -> Option<u32> {
    if provider_prefix_len == 0 || rows.is_empty() {
        return None;
    }
    let messages = rows
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    let (sourced_view, _) = provider_view_with_sources(profile, messages.clone());
    if provider_prefix_len > sourced_view.len() {
        return None;
    }

    let split = sourced_view
        .get(provider_prefix_len)
        .map_or(rows.len(), |item| item.source_index);
    if split == 0 {
        return None;
    }
    let (prefix_view, _) = provider_view(profile, messages[..split].to_vec());
    if prefix_view.len() != provider_prefix_len {
        return None;
    }
    let (suffix_view, _) = provider_view(profile, messages[split..].to_vec());
    let full_view = sourced_view
        .iter()
        .map(|item| &item.message)
        .collect::<Vec<_>>();
    if prefix_view.iter().chain(suffix_view.iter()).eq(full_view) {
        Some(rows[split - 1].0)
    } else {
        None
    }
}

pub fn session_cursor_for_reduction(
    profile: ProviderInputProfile,
    rows: &[(u32, Message)],
    prior_provider_prefix: usize,
    reduction: ExactReduction<'_>,
) -> Result<Option<u32>, CompactionError> {
    let durable_messages = rows
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    let mut full_provider_view = provider_view(profile, durable_messages).0;
    if prior_provider_prefix > full_provider_view.len() {
        return Err(CompactionError::PrefixExceedsHistory);
    }
    let active =
        sanitize_history_for_provider(profile, full_provider_view.split_off(prior_provider_prefix));
    if !reduction
        .compacted_prefix
        .iter()
        .chain(reduction.retained_suffix.iter())
        .eq(active.iter())
    {
        return Err(CompactionError::ExactPartition);
    }

    let cumulative_prefix = prior_provider_prefix
        .checked_add(reduction.compacted_prefix.len())
        .ok_or(CompactionError::PrefixOverflow)?;
    let candidate = compacted_through_sequence(profile, rows, cumulative_prefix);
    Ok(candidate.filter(|cursor| {
        let suffix = rows
            .iter()
            .filter(|(sequence, _)| sequence > cursor)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        provider_view(profile, suffix).0 == reduction.retained_suffix
    }))
}

pub fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    history::pair_safe_boundary(messages, limit)
}

pub fn split_for_summary(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
    provider: crate::backend_provider::BackendProviderKind,
    wire: crate::openai_wire::OpenAiWireApi,
    model: &str,
) -> Result<(Vec<Message>, Vec<Message>), CompactionError> {
    let counter = crate::provider_input::ProviderInputCounter::new(provider, wire, model);
    history::split_messages_for_summary_with_counter(messages, keep_recent_tokens, &counter)
}

/// Requires published canonical messages from the session reconstruction owner.
/// `Compaction.admitted_projection_prefix_stable` proves that a closed raw turn
/// preserves its provider projection under later appends. Persisting a reduction
/// additionally requires `session_cursor_for_reduction` to resolve its exact split.
pub fn session_prefix_is_stable(messages: &[Message]) -> bool {
    messages.last().is_some_and(is_ordinary_message)
}

pub fn safe_to_reduce(messages: &[Message]) -> bool {
    session_prefix_is_stable(messages)
        && sanitize_association_with_sources(messages.to_vec())
            .into_iter()
            .map(|sourced| sourced.message)
            .eq(messages.iter().cloned())
}

fn is_ordinary_message(message: &Message) -> bool {
    match message {
        Message::Assistant { content, .. } => !content
            .iter()
            .any(|item| matches!(item, gents_protocol::message::AssistantContent::ToolCall(_))),
        Message::User { content } => !content
            .iter()
            .any(|item| matches!(item, gents_protocol::message::UserContent::ToolResult(_))),
        Message::System { .. } => false,
    }
}

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
