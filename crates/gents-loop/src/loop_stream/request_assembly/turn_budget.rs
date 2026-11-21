use super::dispatch::TurnContextDecision;
use super::*;
use gents_protocol::error_chain::ErrorChain;

pub(in crate::loop_stream) struct TurnInputs<'a, M> {
    pub(in crate::loop_stream) model: &'a M,
    pub(in crate::loop_stream) tools: &'a [Box<dyn ToolDyn>],
    pub(in crate::loop_stream) config: &'a LoopConfig,
    pub(in crate::loop_stream) turn_index: usize,
}

pub(in crate::loop_stream) struct TurnState<'a> {
    pub(in crate::loop_stream) history: &'a mut Vec<TaggedMessage>,
    pub(in crate::loop_stream) new_messages: &'a mut Vec<TaggedMessage>,
    pub(in crate::loop_stream) replay: &'a mut LoopReplayInput,
    pub(in crate::loop_stream) reduction_chain_keys: &'a mut Vec<String>,
    pub(in crate::loop_stream) active_reduction_keys: &'a mut Vec<String>,
}

pub(in crate::loop_stream) struct RejectedProviderInput {
    pub(in crate::loop_stream) overflow: crate::error::ContextOverflow,
    pub(in crate::loop_stream) input_tokens: usize,
}

struct ReductionPlan {
    admission: crate::compaction::ReductionAdmission,
    provider_budget: usize,
    pre_compaction_tokens: usize,
}

pub(in crate::loop_stream) async fn build_budgeted_request<M>(
    turn: &TurnInputs<'_, M>,
    state: &mut TurnState<'_>,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    let TurnInputs {
        model,
        tools,
        config,
        ..
    } = *turn;
    let request = assemble_provider_request(
        model,
        state.history,
        state.new_messages,
        tools,
        config,
        state.replay,
    )
    .await?;
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
        turn,
        state,
        ReductionPlan {
            admission,
            provider_budget,
            pre_compaction_tokens: context.tokens,
        },
    )
    .await
}

/// Rebuild the turn's request after the provider rejected it as over its
/// context window. The rejected request's provider/estimate ratio becomes the
/// session's calibration, and the budget is the provider's limit
/// (`overflow.limit`, else the configured window) less the output reserve, in
/// this estimator's units at that ratio, so the reduced request is measured
/// against what the provider will count.
pub(in crate::loop_stream) async fn rebuild_after_provider_overflow<M>(
    turn: &TurnInputs<'_, M>,
    state: &mut TurnState<'_>,
    rejected: RejectedProviderInput,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    let TurnInputs {
        model,
        tools,
        config,
        turn_index,
    } = *turn;
    let RejectedProviderInput {
        overflow,
        input_tokens: rejected_input_tokens,
    } = rejected;
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
    let request = assemble_provider_request(
        model,
        state.history,
        state.new_messages,
        tools,
        config,
        state.replay,
    )
    .await?;
    let before_tokens =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?
            .estimated_input_tokens;
    tracing::warn!(
        target: "gents::runtime::agent::loop_stream",
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
        turn,
        state,
        ReductionPlan {
            admission: crate::compaction::ReductionAdmission::required(
                before_tokens,
                estimator_budget,
            ),
            provider_budget,
            pre_compaction_tokens: usize::try_from(observed_input_tokens).unwrap_or(usize::MAX),
        },
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
async fn reduce_and_rebuild<M>(
    turn: &TurnInputs<'_, M>,
    state: &mut TurnState<'_>,
    plan: ReductionPlan,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    let TurnInputs {
        model,
        tools,
        config,
        turn_index,
    } = *turn;
    let ReductionPlan {
        admission,
        provider_budget,
        pre_compaction_tokens,
    } = plan;
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
    let provider_messages = state
        .history
        .iter()
        .chain(state.new_messages.iter())
        .cloned()
        .collect::<Vec<_>>();
    let outcome = compactor(TurnCompactionRequest {
        messages: provider_messages,
        required: state.replay.required.clone(),
        admission,
        turn_index,
        prior_reduction_keys: state.reduction_chain_keys.clone(),
        provider_call: config.provider_call.clone(),
    })
    .await
    .map_err(turn_compaction_failure)?;
    let (mut compacted, staged, reason) = match outcome {
        TurnCompactionOutcome::ProviderViewRepaired { messages } => (
            messages,
            None,
            ContextCompactionReason::ProviderViewRepaired,
        ),
        TurnCompactionOutcome::Reduced {
            messages,
            reduction_key,
            commit,
        } => (
            messages,
            Some((reduction_key, commit)),
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
        state.replay,
    )
    .await?;
    let rebuilt_projection =
        completion_request_input_components(&rebuilt, config.provider_input_counter.as_ref())?;
    let after_tokens = rebuilt_projection.estimated_input_tokens;
    // Provider tokens at the session ratio. Not anchored: the anchor counted
    // the unreduced transcript, which this request no longer extends.
    let after_context_tokens = config.context_calibration.calibrated(after_tokens);
    tracing::info!(
        target: "gents::runtime::agent::loop_stream",
        turn = turn_index,
        before_tokens,
        after_tokens,
        after_context_tokens,
        provider_budget,
        context_window = config.context_window,
        max_output_tokens = config.max_tokens.unwrap_or_default(),
        "compacted provider input before completion dispatch"
    );

    ensure_context_can_dispatch(&rebuilt, config, after_tokens)?;
    if crate::provider_input::budget::threshold_decision(after_context_tokens, provider_budget)
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

    let reduction_key = match staged {
        Some((reduction_key, commit)) => {
            commit.run().await.map_err(turn_compaction_failure)?;
            Some(reduction_key)
        }
        None => None,
    };
    *state.history = rebuilt_history;
    *state.new_messages = rebuilt_new_messages;
    config.context_calibration.invalidate_anchor();
    if let Some(reduction_key) = reduction_key {
        state.reduction_chain_keys.push(reduction_key.clone());
        state.active_reduction_keys.clear();
        state.active_reduction_keys.push(reduction_key);
    }

    Ok((
        rebuilt,
        TurnContextDecision {
            reason,
            pre_compaction_input_tokens: Some(pre_compaction_tokens),
        },
    ))
}

fn turn_compaction_failure(
    error: super::super::TurnCompactionError,
) -> crate::error::LoopStreamError {
    if matches!(
        error,
        super::super::TurnCompactionError::Compaction(
            crate::compaction::CompactionError::Reduction(
                crate::compaction::ReductionError::CannotFit
            ) | crate::compaction::CompactionError::ReplayCheckpoint(_)
                | crate::compaction::CompactionError::ReplayEvidence(_)
        )
    ) {
        return StreamingError::Completion(CompletionError::RequestError(Box::new(error))).into();
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
}
