use std::sync::Arc;

use crate::llm::message::Message;
use anyhow::{Context, Result};
use rig::completion::CompletionModel;

mod history;
mod summary;
#[cfg(test)]
mod tests;

use history::{
    extract_file_activity, pretruncate_tool_results, split_messages_for_summary_with_counter,
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
pub(crate) enum ReductionError {
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
    #[error("compaction_cannot_fit_provider_visible_history")]
    CannotFit,
}

pub(crate) fn rolling_plan_is_valid(
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

pub(crate) fn rolling_step_input<T>(prior_checkpoint: Option<T>, chunk: Vec<T>) -> Vec<T> {
    prior_checkpoint.into_iter().chain(chunk).collect()
}

#[derive(Debug, Clone)]
pub(crate) struct ReductionOptions {
    pub(crate) tool_result_max_chars: usize,
    pub(crate) keep_recent_tokens: usize,
    pub(crate) mode: ReductionMode,
    /// Output budget for the internal summary completion. Deliberately
    /// independent of the user turn's max_output_tokens (#1017).
    pub(crate) summary_max_output_tokens: usize,
    /// Most file paths rendered per list in the formatted summary.
    pub(crate) summary_file_list_max: usize,
    /// The claimed deadline of the request this compaction serves. The
    /// compactor's stored config is daemon-lifetime and carries no deadline,
    /// so this is the only path by which the internal retry ladder's
    /// deadline fail-fast can engage (#1016); `None` leaves recovery bounded
    /// only by the ladder itself.
    pub(crate) deadline: Option<chrono::DateTime<chrono::Utc>>,
    /// Request-scoped provider budget shared with the outer owned loop. This
    /// is crate-private because callers cannot safely mint or replace it.
    pub(crate) aggregate_token_budget: Option<crate::agent::loop_stream::AggregateTokenBudget>,
    /// Effective request seed shared with every provider call in the request.
    pub(crate) sampling_seed: Option<i64>,
}

impl Default for ReductionOptions {
    fn default() -> Self {
        Self {
            tool_result_max_chars: 2000,
            keep_recent_tokens: 20000,
            mode: ReductionMode::Summarize,
            summary_max_output_tokens: crate::config::DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
            summary_file_list_max: crate::config::DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX,
            deadline: None,
            aggregate_token_budget: None,
            sampling_seed: None,
        }
    }
}

/// Proof-carrying admission for the input-driven threshold gate. The reducer
/// cannot be called from production without a value constructed by the one
/// equality-sensitive decision owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReductionAdmission {
    input_tokens: usize,
    effective_input_budget: usize,
    threshold_decision: ThresholdDecision,
}

impl ReductionAdmission {
    pub(crate) fn for_input(
        input_tokens: usize,
        context_window: usize,
        threshold: f64,
    ) -> Option<Self> {
        let effective_input_budget =
            crate::provider_input::budget::effective_input_budget(context_window, threshold);
        matches!(
            threshold_decision(input_tokens, effective_input_budget),
            ThresholdDecision::ReduceEligible
        )
        .then_some(Self {
            input_tokens,
            effective_input_budget,
            threshold_decision: ThresholdDecision::ReduceEligible,
        })
    }

    fn decision<C>(self, can_fit: bool, prefix_len: usize, checkpoint: C) -> ReductionDecision<C> {
        reduction_decision(self.threshold_decision, can_fit, prefix_len, checkpoint)
    }

    pub(crate) fn input_tokens(self) -> usize {
        self.input_tokens
    }

    pub(crate) fn effective_input_budget(self) -> usize {
        self.effective_input_budget
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReductionDecision<C> {
    NotNeeded,
    CannotFit,
    Reduce { prefix_len: usize, checkpoint: C },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReductionOutcome<T, C> {
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

pub(crate) fn reduction_decision<C>(
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

/// Apply a scope-independent reduction decision exactly once. Invalid or empty
/// prefixes fail closed instead of being clamped into a different boundary.
pub(crate) fn apply_reduction_decision<T, C>(
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

#[derive(Debug, Clone)]
pub enum CompactionStrategy {
    StripToolResults,
    Summarize,
    StripThenSummarize,
}

/// Canonical runtime behavior after accepting the durable configuration
/// vocabulary. `Summarize` and `StripThenSummarize` remain valid external
/// spellings, but they do not survive as duplicate branches in the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReductionMode {
    StripOnly,
    Summarize,
}

impl Default for CompactionStrategy {
    fn default() -> Self {
        Self::StripThenSummarize
    }
}

impl CompactionStrategy {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::StripToolResults => "StripToolResults",
            Self::Summarize => "Summarize",
            Self::StripThenSummarize => "StripThenSummarize",
        }
    }

    pub(crate) const fn reduction_mode(&self) -> ReductionMode {
        match self {
            Self::StripToolResults => ReductionMode::StripOnly,
            Self::Summarize | Self::StripThenSummarize => ReductionMode::Summarize,
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
pub(crate) struct ReductionResult {
    state: ReductionResultState,
    pub original_token_estimate: usize,
    pub compacted_token_estimate: usize,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
}

/// Provider-view repair precedes the formal threshold/reduction decision and
/// carries no durable checkpoint. Keeping it outside `ReductionOutcome`
/// prevents an admitted reduction from fabricating the model's `NotNeeded`
/// decision while still allowing tool-result stripping to rescue a request.
#[derive(Debug, Clone)]
enum ReductionResultState {
    ProviderViewRepaired { messages: Vec<Message> },
    Decided(ReductionOutcome<Message, String>),
}

impl ReductionResult {
    pub(crate) fn provider_messages(&self) -> Result<&[Message]> {
        match &self.state {
            ReductionResultState::ProviderViewRepaired { messages } => Ok(messages),
            ReductionResultState::Decided(outcome) => match outcome {
                ReductionOutcome::NotNeeded { messages } => Ok(messages),
                ReductionOutcome::Reduced {
                    retained_suffix, ..
                } => Ok(retained_suffix),
                ReductionOutcome::CannotFit => Err(ReductionError::CannotFit.into()),
            },
        }
    }

    pub(crate) fn exact_reduction(&self) -> Option<ExactReduction<'_>> {
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

    pub(crate) fn cannot_fit(&self) -> bool {
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
pub(crate) struct ExactReduction<'a> {
    pub(crate) compacted_prefix: &'a [Message],
    pub(crate) retained_suffix: &'a [Message],
    pub(crate) checkpoint: &'a str,
}

impl ExactReduction<'_> {
    pub(crate) fn messages_compacted(&self) -> Result<u32> {
        u32::try_from(self.compacted_prefix.len()).map_err(|_| {
            ReductionError::ExactCountOverflow {
                field: "messages_compacted",
                value: self.compacted_prefix.len(),
            }
            .into()
        })
    }

    pub(crate) fn checkpoint_messages(&self) -> Vec<Message> {
        let mut messages = crate::prompt::compaction_summary_message(std::slice::from_ref(
            &self.checkpoint.trim().to_string(),
        ))
        .into_iter()
        .collect::<Vec<_>>();
        messages.extend_from_slice(self.retained_suffix);
        messages
    }
}

pub(crate) trait ReductionEngine: Send + Sync {
    fn reduce(
        &self,
        messages: Vec<Message>,
        context_window: usize,
        options: &ReductionOptions,
        admission: ReductionAdmission,
    ) -> impl std::future::Future<Output = Result<ReductionResult>> + Send;
}

#[derive(Clone)]
pub(crate) struct ProviderReductionEngine<M: CompletionModel> {
    model: Arc<M>,
    config: crate::agent::loop_stream::LoopConfig,
    now: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
}

impl<M: CompletionModel> ProviderReductionEngine<M> {
    pub(crate) fn new(model: Arc<M>, mut config: crate::agent::loop_stream::LoopConfig) -> Self {
        // Compaction is an internal, non-persisting sub-completion, not a user
        // execution origin; it must not inherit the parent's retry ladder (which
        // for scheduled origins is a deadline-less 5s/30s/120s backoff that would
        // block inline compaction for minutes) (#648). But it has no caller-level
        // retry either, so zero recovery made one empty provider turn abort the
        // whole user request: use the bounded immediate internal budget (#1016).
        config.retry_policy =
            crate::agent::completion_retry::CompletionRetryPolicy::internal_immediate();
        Self {
            model,
            config,
            now: Arc::new(chrono::Utc::now),
        }
    }

    /// Compute the one retention policy used by both session-entry and
    /// between-turn compaction with this compactor's provider projection.
    pub(crate) fn retention_target(
        &self,
        configured_keep_recent: usize,
        compactable_messages: &[Message],
        admission: ReductionAdmission,
    ) -> Result<usize> {
        let compactable_input_tokens = self
            .config
            .provider_input_counter
            .estimate_message_request(compactable_messages)
            .context("projecting compactable provider messages")?;
        let fixed_input_tokens = admission
            .input_tokens()
            .saturating_sub(compactable_input_tokens);
        Ok(compaction_retention_target(
            configured_keep_recent,
            admission.effective_input_budget(),
            fixed_input_tokens,
        ))
    }

    #[cfg(test)]
    fn with_now(
        mut self,
        now: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
    ) -> Self {
        self.now = now;
        self
    }
}

impl<M: CompletionModel + 'static> ReductionEngine for ProviderReductionEngine<M> {
    async fn reduce(
        &self,
        messages: Vec<Message>,
        context_window: usize,
        options: &ReductionOptions,
        admission: ReductionAdmission,
    ) -> Result<ReductionResult> {
        let counter = self.config.provider_input_counter.as_ref();
        let original_token_estimate = counter
            .estimate_message_request(&messages)
            .context("projecting original compaction input")?;

        // Narrow once to the canonical provider view. Every downstream split,
        // checkpoint, durable count, and returned suffix is expressed in this
        // one coordinate system; callers never reconstruct it from a scalar.
        let (stripped_messages, stripped_activity) = provider_view(messages);

        let stripped_token_estimate = counter
            .estimate_message_request(&stripped_messages)
            .context("projecting normalized compaction input")?;
        // Admission owns the complete provider-input threshold decision. The
        // reducer receives only eligible work and never rechecks a narrower
        // history-only estimate.
        if matches!(options.mode, ReductionMode::StripOnly) {
            return Ok(ReductionResult {
                state: ReductionResultState::provider_view_repaired(stripped_messages),
                original_token_estimate,
                compacted_token_estimate: stripped_token_estimate,
                files_read: stripped_activity.files_read,
                files_modified: stripped_activity.files_modified,
            });
        }

        let (old_messages, recent_messages) = split_messages_for_summary_with_counter(
            stripped_messages.clone(),
            options.keep_recent_tokens,
            counter,
        )?;
        if old_messages.is_empty() {
            // Provider-view stripping is itself a useful non-durable repair.
            // The caller rebuilds and projects the complete request before it
            // decides whether that repair satisfied the threshold. If the
            // provider-visible estimate did not improve, the indivisible
            // canonical view cannot fit this admitted reduction.
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

        // The transcript is untrusted source material. Put the summarization
        // contract in the system layer so an unfinished user/tool workflow in
        // `prepared_history` cannot outrank it and turn the internal completion
        // into a continuation of the old task. The final user message is
        // deliberately neutral; it carries no executable transcript content.
        let mut summary_config = self.config.clone();
        summary_config.context_window = context_window;
        summary_config.preamble = Some(compaction_prompt().to_string());
        summary_config.context_message = None;
        summary_config.tool_choice = None;
        summary_config.turn_compactor = None;
        summary_config.max_turns = 0;
        // The summary completion has its own output budget, deliberately
        // independent of the user turn's max_output_tokens (#1017): a large
        // turn budget must not let the model balloon the internal summary.
        let configured_summary_max_output_tokens = options
            .summary_max_output_tokens
            .clamp(1, crate::config::MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS);
        let summary_max_output_tokens =
            summary_output_ceiling(configured_summary_max_output_tokens, context_window);
        summary_config.max_tokens = Some(summary_max_output_tokens as u64);
        summary_config.aggregate_token_budget = options.aggregate_token_budget.clone();
        summary_config.additional_params = crate::completion_factory::merge_optional_params(
            summary_config.additional_params.take(),
            options
                .sampling_seed
                .map(|seed| serde_json::json!({ "seed": seed })),
        );
        // Both deadlines are hard stops when present; recovery must respect
        // the earlier one.
        summary_config.deadline = match (options.deadline, summary_config.deadline) {
            (Some(from_options), Some(from_config)) => Some(from_options.min(from_config)),
            (from_options, from_config) => from_options.or(from_config),
        };
        summary_config.structured_output = Some(
            crate::agent::loop_stream::StructuredOutputConfig::for_type::<ContinuationCheckpoint>(),
        );

        // Roll the selected old prefix entirely in memory. Each successful step
        // feeds its checkpoint into the next bounded pair-safe chunk; the daemon
        // persists only the one final result after this function returns.
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
            // Every rolling step is a fresh owned loop. Its retry controller
            // checks the deadline after a failed attempt, so guard the initial
            // attempt here as well: a prior chunk may have consumed the shared
            // request deadline while producing a valid checkpoint.
            if deadline_elapsed(summary_config.deadline, self.now.as_ref()) {
                return Err(ReductionError::DeadlineElapsed.into());
            }
            let remaining = &old_messages[consumed..];
            let (chunk_len, chunk_input_tokens) = largest_fitting_summary_chunk(
                self.model.as_ref(),
                remaining,
                rolling_summary.as_deref(),
                &summary_config,
                options,
                consumed,
                context_window,
            )
            .await?;
            let pair_closed = history::pair_safe_boundaries(remaining).contains(&chunk_len);
            let configured_output = configured_output_ceiling(summary_config.max_tokens);
            let chunk = pretruncate_tool_results(
                remaining[..chunk_len].to_vec(),
                options.tool_result_max_chars,
            );
            let prior_checkpoint = rolling_summary.as_ref().and_then(|summary| {
                crate::prompt::compaction_summary_message(std::slice::from_ref(summary))
            });
            let prepared_history = rolling_step_input(prior_checkpoint, chunk);
            let checkpoint = summarize_checkpoint(
                self.model.as_ref(),
                prepared_history,
                summary_config.clone(),
                self.now.clone(),
            )
            .await?;
            consumed =
                consumed
                    .checked_add(chunk_len)
                    .ok_or(ReductionError::ExactCountOverflow {
                        field: "messages_compacted",
                        value: old_messages.len(),
                    })?;
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
                    .clamp(1, crate::config::MAX_COMPACTION_SUMMARY_FILE_LIST_MAX),
            )));
        }

        let summary = rolling_summary
            .expect("non-empty old prefix produces a checkpoint")
            .trim()
            .to_string();
        if summary.is_empty() {
            return Err(ReductionError::InvalidRollingPlan.into());
        }
        if !rolling_plan_is_valid(
            old_messages.len(),
            &chunk_messages,
            &chunk_pair_closed,
            &chunk_can_dispatch,
            consumed,
        ) {
            return Err(ReductionError::InvalidRollingPlan.into());
        }
        let mut compacted_projection =
            crate::prompt::compaction_summary_message(std::slice::from_ref(&summary))
                .into_iter()
                .collect::<Vec<_>>();
        compacted_projection.extend(recent_messages.iter().cloned());
        let compacted_token_estimate = counter
            .estimate_message_request(&compacted_projection)
            .context("projecting final compacted provider input")?;
        let prefix_len = old_messages.len();
        u32::try_from(prefix_len).map_err(|_| ReductionError::ExactCountOverflow {
            field: "messages_compacted",
            value: prefix_len,
        })?;
        let outcome = apply_reduction_decision(
            stripped_messages,
            admission.decision(true, prefix_len, summary),
        );
        if matches!(outcome, ReductionOutcome::CannotFit) {
            return Err(ReductionError::InvalidRollingPlan.into());
        }
        Ok(ReductionResult {
            state: ReductionResultState::decided(outcome),
            original_token_estimate,
            compacted_token_estimate,
            files_read,
            files_modified,
        })
    }
}

async fn largest_fitting_summary_chunk<M: CompletionModel>(
    model: &M,
    remaining: &[Message],
    prior_summary: Option<&str>,
    summary_config: &crate::agent::loop_stream::LoopConfig,
    options: &ReductionOptions,
    absolute_start: usize,
    context_window: usize,
) -> Result<(usize, usize)> {
    let boundaries = history::pair_safe_boundaries(remaining);
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
        // Do not pack a summary request all the way to the raw context edge:
        // doing so is technically dispatchable but can reduce a structured
        // checkpoint to a one-token output allowance. The rolling summary
        // budget reserves the configured summary ceiling whenever it can
        // coexist with input. The owned loop still applies the dynamic output
        // clamp immediately before dispatch.
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

async fn summary_candidate_tokens<M: CompletionModel>(
    model: &M,
    chunk: &[Message],
    prior_summary: Option<&str>,
    summary_config: &crate::agent::loop_stream::LoopConfig,
    options: &ReductionOptions,
) -> Result<usize> {
    let prepared_chunk = pretruncate_tool_results(chunk.to_vec(), options.tool_result_max_chars);
    let prior_checkpoint = prior_summary.and_then(|summary| {
        let summaries = [summary.to_string()];
        crate::prompt::compaction_summary_message(&summaries)
    });
    let history = rolling_step_input(prior_checkpoint, prepared_chunk);
    let request = crate::agent::loop_stream::build_request(
        model,
        Message::user(compaction_request_prompt()),
        &history,
        &[],
        &[],
        summary_config,
    )
    .await
    .map_err(anyhow::Error::new)?;
    Ok(summary_config
        .provider_input_counter
        .estimate_request(&request)
        .context("projecting rolling compaction summary request")?)
}

async fn summarize_checkpoint<M: CompletionModel + 'static>(
    model: &M,
    prepared_history: Vec<Message>,
    summary_config: crate::agent::loop_stream::LoopConfig,
    now: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
) -> Result<ContinuationCheckpoint> {
    // Candidate construction is asynchronous. Re-check at the actual owned
    // loop boundary so expiry during projection cannot leak one provider call.
    if deadline_elapsed(summary_config.deadline, now.as_ref()) {
        return Err(ReductionError::DeadlineElapsed.into());
    }
    let guided_result = crate::agent::loop_stream::run_loop_to_typed(
        model.clone(),
        None,
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
                error = %bounded_error_diagnostic(&format!("{guided_error:#}")),
                "guided compaction output exhausted recovery; trying one strict non-guided JSON fallback"
            );
            let mut fallback_config = summary_config;
            fallback_config.structured_output = None;
            fallback_config.on_rendered_request =
                Some(crate::rendered_request::scope::ambient_arming_sink(
                    crate::rendered_request::CaptureScopeKind::CompactionFallback,
                ));
            fallback_config.retry_policy =
                crate::agent::completion_retry::CompletionRetryPolicy::no_retry();
            fallback_config.preamble = Some(format!(
                "{}\n\n{}",
                compaction_prompt(),
                compaction_json_fallback_prompt()
            ));
            let raw = crate::agent::loop_stream::run_loop_to_text(
                model.clone(),
                None,
                Message::user(compaction_request_prompt()),
                prepared_history,
                std::sync::Arc::new(Vec::new()),
                fallback_config,
            )
            .await
            .map_err(|fallback_error| {
                if crate::agent::loop_stream::aggregate_token_budget_exhaustion_message(
                    &fallback_error,
                )
                .is_some()
                {
                    fallback_error.context(
                        "non-guided compaction fallback exhausted the request token budget",
                    )
                } else {
                    anyhow::anyhow!(
                        "compaction_provider_failure: guided structured output failed: {}; \
                         non-guided JSON fallback failed: {}",
                        bounded_error_diagnostic(&format!("{guided_error:#}")),
                        bounded_error_diagnostic(&format!("{fallback_error:#}"))
                    )
                }
            })?;
            parse_fallback_checkpoint(&raw).map_err(|fallback_error| {
                anyhow::anyhow!(
                    "compaction_provider_failure: guided structured output failed: {}; \
                     non-guided JSON fallback failed: {}",
                    bounded_error_diagnostic(&format!("{guided_error:#}")),
                    bounded_error_diagnostic(&fallback_error)
                )
            })
        }
        Err(error) => {
            if crate::agent::loop_stream::aggregate_token_budget_exhaustion_message(&error)
                .is_some()
            {
                return Err(error.context("guided compaction exhausted the request token budget"));
            }
            let deadline_context = if deadline_elapsed(summary_config.deadline, now.as_ref()) {
                "compaction deadline elapsed after "
            } else {
                ""
            };
            Err(anyhow::anyhow!(
                "compaction_provider_failure: {deadline_context}{}",
                bounded_error_diagnostic(&format!("{error:#}"))
            ))
        }
    }
}

fn is_guided_output_failure(error: &anyhow::Error) -> bool {
    let diagnostic = format!("{error:#}");
    diagnostic.contains("structured-output validation failed")
        || diagnostic.contains("completion produced no visible output")
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

pub fn sanitize_history_for_provider(messages: Vec<Message>) -> Vec<Message> {
    history::strip_idless_reasoning(history::normalize_assistant_content_order(
        history::drop_unpaired_tool_calls(history::drop_orphaned_tool_results(messages)),
    ))
}

/// The single canonical narrowing from the durable transcript to the provider
/// view: stub tool-result payloads, then drop unpaired calls and orphaned
/// results and normalize assistant content order.
///
/// Both sides of compaction's prefix accounting index *this* list. The
/// compaction writer records `messages_compacted` against it and the request
/// reader drops that many rows from it; measuring in one space and dropping in
/// another was defect 3 of #993.
///
/// Modelled as `Compaction.providerView`, proven idempotent by
/// `Compaction.providerView_idempotent` — which is what lets [`ReductionEngine::reduce`]
/// re-normalize its own input for free.
pub fn provider_view(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    let (stripped, activity) = strip_tool_results(messages);
    (sanitize_history_for_provider(stripped), activity)
}

fn provider_view_with_sources(
    messages: Vec<Message>,
) -> (Vec<history::SourcedMessage>, FileActivity) {
    let sourced = messages
        .into_iter()
        .enumerate()
        .map(|(source_index, message)| history::SourcedMessage {
            source_index,
            message,
        })
        .collect();
    let (stripped, activity) = history::strip_tool_results_sourced(sourced);
    let sanitized = history::strip_idless_reasoning_sourced(
        history::normalize_assistant_content_order_sourced(
            history::drop_unpaired_tool_calls_sourced(history::drop_orphaned_tool_results_sourced(
                stripped,
            )),
        ),
    );
    (sanitized, activity)
}

/// Project the active tail from a canonical provider view after applying the
/// durable compacted-prefix count. Re-sanitization preserves compatibility
/// with legacy entries whose prefix could end inside a tool turn.
pub fn active_provider_history(
    mut provider_history: Vec<Message>,
    compacted_messages: usize,
) -> Result<Vec<Message>> {
    anyhow::ensure!(
        compacted_messages <= provider_history.len(),
        "session compaction prefix exceeds the canonical provider history: compacted={}, available={}",
        compacted_messages,
        provider_history.len()
    );
    let active = provider_history.split_off(compacted_messages);
    Ok(sanitize_history_for_provider(active))
}

/// Find the inclusive canonical message cursor that denotes a provider-view
/// prefix of exactly `provider_prefix_len` messages.
///
/// The canonical projection carries each retained message's raw source index,
/// which identifies the one possible raw boundary for a provider prefix. That
/// candidate is then independently projected on both sides to establish the
/// runtime witness for `Compaction.CursorDenotes`. The work is linear in the
/// transcript rather than re-projecting every possible split.
pub fn compacted_through_sequence(
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
    let (sourced_view, _) = provider_view_with_sources(messages.clone());
    if provider_prefix_len > sourced_view.len() {
        return None;
    }

    let split = sourced_view
        .get(provider_prefix_len)
        .map_or(rows.len(), |item| item.source_index);
    if split == 0 {
        return None;
    }
    let (prefix_view, _) = provider_view(messages[..split].to_vec());
    if prefix_view.len() != provider_prefix_len {
        return None;
    }
    let (suffix_view, _) = provider_view(messages[split..].to_vec());
    let full_view = sourced_view
        .iter()
        .map(|item| &item.message)
        .collect::<Vec<_>>();
    if prefix_view
        .iter()
        .chain(suffix_view.iter())
        .eq(full_view.into_iter())
    {
        Some(rows[split - 1].0)
    } else {
        None
    }
}

/// Resolve the session-specific raw cursor for one exact provider reduction.
/// Persistence-specific prefix offsets are contained here; request assembly
/// and the shared reducer deal only in canonical provider views.
pub(crate) fn session_cursor_for_reduction(
    rows: &[(u32, Message)],
    prior_provider_prefix: usize,
    reduction: ExactReduction<'_>,
) -> Result<Option<u32>> {
    let durable_messages = rows
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    let mut full_provider_view = provider_view(durable_messages).0;
    anyhow::ensure!(
        prior_provider_prefix <= full_provider_view.len(),
        "session compaction prefix exceeds the canonical provider history"
    );
    let active = sanitize_history_for_provider(full_provider_view.split_off(prior_provider_prefix));
    anyhow::ensure!(
        reduction
            .compacted_prefix
            .iter()
            .chain(reduction.retained_suffix.iter())
            .eq(active.iter()),
        "exact reduction does not partition the active session provider view"
    );

    let cumulative_prefix = prior_provider_prefix
        .checked_add(reduction.compacted_prefix.len())
        .context("session compaction provider-prefix count overflow")?;
    let candidate = compacted_through_sequence(rows, cumulative_prefix);
    Ok(candidate.filter(|cursor| {
        let suffix = rows
            .iter()
            .filter(|(sequence, _)| sequence > cursor)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        provider_view(suffix).0 == reduction.retained_suffix
    }))
}

/// Greatest `j <= limit` at which no tool call is awaiting its result — the
/// index [`ReductionEngine::reduce`] retreats its token-budget split to.
///
/// Re-exported so the generated conformance cases can check the production
/// boundary against `Compaction.pairSafeBoundary`.
pub fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    history::pair_safe_boundary(messages, limit)
}

/// The real split [`ReductionEngine::reduce`] performs: everything before the
/// boundary is summarized, everything from it is retained.
///
/// Exported so the generated conformance cases can sweep budgets against the
/// live splitter rather than only against [`pair_safe_boundary`] — otherwise a
/// change that stopped *calling* the boundary would slip through.
pub fn split_for_summary(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
    provider: crate::BackendProviderKind,
    wire: crate::OpenAiWireApi,
    model: &str,
) -> Result<(Vec<Message>, Vec<Message>)> {
    let counter = crate::provider_input::ProviderInputCounter::new(provider, wire, model);
    history::split_messages_for_summary_with_counter(messages, keep_recent_tokens, &counter)
}

/// Mirror of Lean `StreamingResponse.Status`, with the same terminal partition,
/// so generated conformance cases can be fed straight in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Streaming,
    Complete,
    Error,
}

impl ResponseStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Error)
    }

    pub fn from_defra(value: &str) -> Option<Self> {
        match value {
            "streaming" => Some(Self::Streaming),
            "complete" => Some(Self::Complete),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Resolves the streaming status of the response that produced a message.
pub trait ResponseStatusIndex {
    fn status_of(&self, message: &Message) -> Option<ResponseStatus>;
}

/// Every response in scope is terminal.
pub struct AllTerminal;

impl ResponseStatusIndex for AllTerminal {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        Some(ResponseStatus::Complete)
    }
}

/// No status is known — the conservative resolution when anything in scope is
/// still streaming.
pub struct NoneKnown;

impl ResponseStatusIndex for NoneKnown {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        None
    }
}

/// Runtime counterpart of Lean `PromptView.safeToReduce`: a transcript may only
/// be reduced when every tool result it retains belongs to a response whose
/// status is known and terminal. Reducing under a live response can summarize
/// away a turn that is still being written.
///
/// See `boundary.compaction.safe-to-reduce-session-scope` for how the daemon
/// resolves statuses at session scope rather than per message.
pub fn safe_to_reduce(messages: &[Message], statuses: &impl ResponseStatusIndex) -> bool {
    messages.iter().all(|message| {
        if !carries_tool_result(message) {
            return true;
        }
        statuses
            .status_of(message)
            .is_some_and(ResponseStatus::is_terminal)
    })
}

/// Runtime counterpart of Lean `PromptAssembly.UniqueCallIds`: no tool-call id
/// is announced by more than one assistant message.
///
/// This is a *hypothesis* of the prefix-stability theorem, not a structural
/// guarantee — call ids come from the provider, and nothing in the ingestion
/// path enforces uniqueness across a session. `sanitize_history_for_provider`
/// credits an announcement from the globally resolved set, so a later turn that
/// reuses an id resurrects an earlier announcement the shorter view had dropped
/// as unpaired. The prefix then changes under append and a stored
/// `messages_compacted` no longer names the rows it was measured against.
/// `Compaction.reused_call_id_breaks_prefix_stability` exhibits exactly that.
///
/// Checking it here turns the theorem's hypothesis into a precondition the
/// runtime verifies before recording a count. See
/// `boundary.compaction.unique-call-ids-checked` for the residual gap and the
/// follow-up that would remove the need for the check.
pub fn has_unique_call_ids(messages: &[Message]) -> bool {
    let mut announced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in messages {
        let Message::Assistant { content, .. } = message else {
            continue;
        };
        // Per-message first: Lean models a turn's ids as a `Finset`, so a repeat
        // *within* one announcement collapses rather than conflicting.
        let this_turn: std::collections::HashSet<String> = content
            .iter()
            .filter_map(|item| match item {
                crate::llm::message::AssistantContent::ToolCall(tool_call) => Some(
                    tool_call
                        .call_id
                        .clone()
                        .unwrap_or_else(|| tool_call.id.clone()),
                ),
                _ => None,
            })
            .collect();
        for call_id in this_turn {
            if !announced.insert(call_id) {
                return false;
            }
        }
    }
    true
}

fn carries_tool_result(message: &Message) -> bool {
    let Message::User { content } = message else {
        return false;
    };
    content
        .iter()
        .any(|item| matches!(item, crate::llm::message::UserContent::ToolResult(_)))
}

pub fn bounded_summary(summary: String) -> String {
    let (bounded, _, _) = crate::truncation::truncate_text(
        &summary,
        crate::truncation::TruncationMode::Head,
        &crate::truncation::TruncationLimits::default(),
    );
    bounded
}
