use std::sync::Arc;

use gents_protocol::error_chain::ErrorChain;
use gents_protocol::message::Message;

use super::history::{self, pretruncate_tool_results};
use super::summary::{
    bounded_error_diagnostic, compaction_json_fallback_prompt, compaction_request_prompt,
    parse_fallback_checkpoint, ContinuationCheckpoint,
};
use super::{rolling_step_input, CompactionError, ReductionError, ReductionOptions};
use crate::provider_input::budget::{configured_output_ceiling, rolling_summary_input_budget};
use crate::rig_compat::ProviderModel;

/// The longest pair-safe prefix of `remaining`, at most `max_messages` long,
/// whose summary request fits the calibrated input budget.
#[allow(clippy::too_many_arguments)]
pub(super) async fn largest_fitting_summary_chunk<M>(
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

pub(super) async fn summarize_checkpoint<M>(
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

pub(super) fn deadline_elapsed(
    deadline: Option<chrono::DateTime<chrono::Utc>>,
    now: &(dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync),
) -> bool {
    deadline.is_some_and(|deadline| now() >= deadline)
}
