use std::sync::Arc;

use futures::future::BoxFuture;
use gents_protocol::message::Message;

use super::history::{
    self, extract_file_activity, pretruncate_tool_results,
    split_messages_for_summary_with_counter_bounded,
};
use super::reduction::ReductionResultState;
use super::rolling::{deadline_elapsed, largest_fitting_summary_chunk, summarize_checkpoint};
use super::summary::{compaction_prompt, format_summary, ContinuationCheckpoint};
use super::{
    apply_reduction_decision, bounded_summary, merge_optional_params, provider_view,
    rolling_plan_is_valid, rolling_step_input, CompactionError, FileActivity, ReductionAdmission,
    ReductionError, ReductionMode, ReductionOptions, ReductionOutcome, ReductionResult, Result,
    MAX_CLAUDE_GUIDED_SUMMARY_RETRIES, MAX_COMPACTION_SUMMARY_FILE_LIST_MAX,
    MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS,
};
use crate::provider_input::budget::{
    compaction_retention_target, configured_output_ceiling, summary_output_ceiling,
};
use crate::rig_compat::ProviderModel;

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
