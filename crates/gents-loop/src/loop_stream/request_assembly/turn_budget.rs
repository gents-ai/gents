use super::dispatch::TurnContextDecision;
use super::*;
use gents_protocol::error_chain::ErrorChain;

#[allow(clippy::too_many_arguments)]
pub(in crate::loop_stream) async fn build_budgeted_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    let request =
        assemble_provider_request(model, history, new_messages, tools, config, replay).await?;
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    let before_tokens = projection.estimated_input_tokens;

    if config.turn_compactor.is_none() {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::CompactorUnavailable,
                pre_compaction_input_tokens: None,
            },
        ));
    }
    let provider_budget = crate::provider_input::budget::compaction_input_budget(
        config.context_window,
        config.compaction_threshold,
        crate::provider_input::budget::configured_output_ceiling(config.max_tokens),
    );
    let context = config.context_calibration.context(before_tokens);
    let Some(admission) =
        crate::compaction::ReductionAdmission::for_context(context, provider_budget)
    else {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::BelowThreshold,
                pre_compaction_input_tokens: None,
            },
        ));
    };
    reduce_and_rebuild(
        model,
        history,
        new_messages,
        tools,
        config,
        replay,
        turn_index,
        reduction_chain_keys,
        active_reduction_keys,
        admission,
        provider_budget,
        context.tokens,
    )
    .await
}

/// Rebuild the turn's request after the provider rejected it as over its
/// context window. The rejected request's provider/estimate ratio becomes the
/// session's calibration, and the budget is the provider's limit
/// (`overflow.limit`, else the configured window) less the output reserve, in
/// this estimator's units at that ratio, so the reduced request is measured
/// against what the provider will count.
#[allow(clippy::too_many_arguments)]
pub(in crate::loop_stream) async fn rebuild_after_provider_overflow<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
    overflow: crate::error::ContextOverflow,
    rejected_input_tokens: usize,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    use crate::provider_input::budget;
    let provider_limit = overflow
        .limit
        .and_then(|limit| usize::try_from(limit).ok())
        .map_or(config.context_window, |limit| {
            limit.min(config.context_window)
        });
    let observed_input_tokens = overflow.input_tokens.map_or_else(
        || {
            u64::try_from(provider_limit)
                .unwrap_or(u64::MAX)
                .saturating_add(1)
        },
        u64::from,
    );
    config
        .context_calibration
        .observe(observed_input_tokens, rejected_input_tokens);
    let ratio = config.context_calibration.ratio_basis_points();
    let provider_budget = budget::compaction_input_budget(
        provider_limit,
        config.compaction_threshold,
        budget::configured_output_ceiling(config.max_tokens),
    );
    let estimator_budget = config.context_calibration.estimator_budget(provider_budget);
    let request =
        assemble_provider_request(model, history, new_messages, tools, config, replay).await?;
    let before_tokens =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?
            .estimated_input_tokens;
    tracing::warn!(
        target: "gents::agent::loop_stream",
        turn = turn_index,
        provider_limit,
        observed_input_tokens,
        rejected_input_tokens,
        ratio_basis_points = ratio,
        provider_budget,
        estimator_budget,
        "provider rejected the request as over its context window; compacting once"
    );
    reduce_and_rebuild(
        model,
        history,
        new_messages,
        tools,
        config,
        replay,
        turn_index,
        reduction_chain_keys,
        active_reduction_keys,
        crate::compaction::ReductionAdmission::required(before_tokens, estimator_budget),
        provider_budget,
        usize::try_from(observed_input_tokens).unwrap_or(usize::MAX),
    )
    .await
}

/// Run the turn compactor under `admission` and rebuild the request. The
/// rebuilt request, in provider tokens (calibrated: the anchor describes the
/// unreduced transcript), must fit `provider_budget`, the budget that
/// admitted the reduction. `pre_compaction_tokens` is the unreduced
/// request's size in provider tokens, for the accounting.
///
/// The transcript, replay keys and anchor change only when the rebuilt
/// request fits: a reduction that fails, or cannot fit, leaves the session as
/// it was.
#[allow(clippy::too_many_arguments)]
async fn reduce_and_rebuild<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
    admission: crate::compaction::ReductionAdmission,
    provider_budget: usize,
    pre_compaction_tokens: usize,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    let Some(compactor) = config.turn_compactor.as_ref() else {
        return Err(StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "provider input must be reduced but this completion loop has no per-turn compactor",
            ),
        )))
        .into());
    };
    let before_tokens = admission.input_tokens();
    let provider_messages = history
        .iter()
        .chain(new_messages.iter())
        .cloned()
        .collect::<Vec<_>>();
    let outcome = compactor(TurnCompactionRequest {
        messages: provider_messages,
        required: replay.required.clone(),
        admission,
        turn_index,
        prior_reduction_keys: reduction_chain_keys.clone(),
    })
    .await
    .map_err(|error| {
        if matches!(
            error,
            super::TurnCompactionError::Compaction(
                crate::compaction::CompactionError::Reduction(
                    crate::compaction::ReductionError::CannotFit
                ) | crate::compaction::CompactionError::ReplayCheckpoint(_)
                    | crate::compaction::CompactionError::ReplayEvidence(_)
            )
        ) {
            return StreamingError::Completion(CompletionError::RequestError(Box::new(error)))
                .into();
        }
        error.aggregate_token_budget_exhaustion().map_or_else(
            || {
                StreamingError::Completion(CompletionError::ProviderError(format!(
                    "per-turn provider-input compaction failed: {}",
                    ErrorChain(&error)
                )))
                .into()
            },
            |reason| {
                crate::rig_compat::loop_verdict_error(
                    crate::error::LoopVerdict::AggregateTokenBudgetExhausted,
                    reason,
                )
            },
        )
    })?;
    let (mut compacted, reduction_key, reason) = match outcome {
        TurnCompactionOutcome::ProviderViewRepaired { messages } => (
            messages,
            None,
            ContextCompactionReason::ProviderViewRepaired,
        ),
        TurnCompactionOutcome::Reduced {
            messages,
            reduction_key,
        } => (
            messages,
            Some(reduction_key),
            ContextCompactionReason::Compacted,
        ),
        TurnCompactionOutcome::CannotFit => {
            return Err(
                StreamingError::Completion(CompletionError::RequestError(Box::new(
                    crate::compaction::ReductionError::CannotFit,
                )))
                .into(),
            );
        }
    };
    let compacted_prompt = compacted.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "per-turn provider-input compaction returned no prompt",
            ),
        )))
    })?;
    let mut rebuilt_history = compacted;
    let mut rebuilt_new_messages = vec![compacted_prompt];
    let rebuilt = assemble_provider_request(
        model,
        &mut rebuilt_history,
        &mut rebuilt_new_messages,
        tools,
        config,
        replay,
    )
    .await?;
    let rebuilt_projection =
        completion_request_input_components(&rebuilt, config.provider_input_counter.as_ref())?;
    let after_tokens = rebuilt_projection.estimated_input_tokens;
    // Provider tokens at the session ratio. Not anchored: the anchor counted
    // the unreduced transcript, which this request no longer extends.
    let after_context_tokens = config.context_calibration.calibrated(after_tokens);
    tracing::info!(
        target: "gents::agent::loop_stream",
        turn = turn_index,
        before_tokens,
        after_tokens,
        after_context_tokens,
        provider_budget,
        context_window = config.context_window,
        max_output_tokens = config.max_tokens.unwrap_or_default(),
        "compacted provider input before completion dispatch"
    );

    let rebuilt_can_dispatch = crate::provider_input::budget::can_dispatch(
        after_context_tokens,
        config.context_window,
        crate::provider_input::budget::configured_output_ceiling(rebuilt.max_tokens),
    );
    if rebuilt_can_dispatch
        && crate::provider_input::budget::threshold_decision(after_context_tokens, provider_budget)
            == crate::provider_input::budget::ThresholdDecision::ReduceEligible
    {
        tracing::warn!(
            estimated_input_tokens = after_tokens,
            after_context_tokens,
            provider_budget,
            "provider input remains over threshold after reduction"
        );
        return Err(
            StreamingError::Completion(CompletionError::RequestError(Box::new(
                crate::compaction::ReductionError::CannotFit,
            )))
            .into(),
        );
    }

    *history = rebuilt_history;
    *new_messages = rebuilt_new_messages;
    config.context_calibration.invalidate_anchor();
    if let Some(reduction_key) = reduction_key {
        reduction_chain_keys.push(reduction_key.clone());
        active_reduction_keys.clear();
        active_reduction_keys.push(reduction_key);
    }

    Ok((
        rebuilt,
        TurnContextDecision {
            reason,
            pre_compaction_input_tokens: Some(pre_compaction_tokens),
        },
    ))
}
