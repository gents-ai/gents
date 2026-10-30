use super::*;
use gents_protocol::error_chain::ErrorChain;

pub fn completion_request_input_components(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<crate::provider_input::ProviderInputProjection, StreamingError> {
    counter.project_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("provider_input_projection_failed: {}", ErrorChain(&error)),
            ),
        )))
    })
}

fn completion_request_input_tokens(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<usize, StreamingError> {
    counter.estimate_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("provider_input_projection_failed: {}", ErrorChain(&error)),
            ),
        )))
    })
}

#[derive(Clone, Debug)]
pub(in crate::loop_stream) struct PreparedDispatch {
    request: CompletionRequest,
    projection: crate::provider_input::ProviderInputProjection,
    context: crate::provider_input::budget::ContextEstimate,
}

impl PreparedDispatch {
    pub(in crate::loop_stream) fn request(&self) -> &CompletionRequest {
        &self.request
    }

    pub(in crate::loop_stream) fn projection(
        &self,
    ) -> &crate::provider_input::ProviderInputProjection {
        &self.projection
    }

    /// The request's size in provider tokens under the calibration and
    /// anchor in effect when this attempt was prepared.
    pub(in crate::loop_stream) fn context(&self) -> crate::provider_input::budget::ContextEstimate {
        self.context
    }
}

pub(in crate::loop_stream) fn prepare_dispatch_attempt(
    assembled_request: &CompletionRequest,
    config: &LoopConfig,
    aggregate_token_budget: Option<&AggregateTokenBudget>,
) -> Result<PreparedDispatch, crate::error::LoopStreamError> {
    let mut request = assembled_request.clone();
    let estimated_input_tokens =
        completion_request_input_tokens(&request, config.provider_input_counter.as_ref())?;
    let context = config.context_calibration.context(estimated_input_tokens);
    // The calibrated count triggers compaction and caps output, but only the
    // raw count can refuse dispatch: the provider's own overflow rejection is
    // recovered, an overestimate is not.
    let configured_max_tokens = request.max_tokens;
    clamp_request_output_budget(&mut request, config, context.tokens);
    if request.max_tokens == Some(0) {
        request.max_tokens = configured_max_tokens;
        clamp_request_output_budget(&mut request, config, estimated_input_tokens);
    }
    ensure_context_can_dispatch(&request, config, estimated_input_tokens)?;
    super::aggregate_budget::clamp_request_aggregate_token_budget(
        &mut request,
        aggregate_token_budget,
        &context,
    )?;
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    debug_assert_eq!(projection.estimated_input_tokens, estimated_input_tokens);
    Ok(PreparedDispatch {
        request,
        projection,
        context,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::loop_stream) struct TurnContextDecision {
    pub(in crate::loop_stream) reason: ContextCompactionReason,
    pub(in crate::loop_stream) pre_compaction_input_tokens: Option<usize>,
}

pub(in crate::loop_stream) fn context_accounting_for_request(
    dispatch: &PreparedDispatch,
    config: &LoopConfig,
    turn_index: usize,
    attempt: u32,
    compaction_reason: ContextCompactionReason,
    pre_compaction_input_tokens: Option<usize>,
) -> ContextAccounting {
    let request = dispatch.request();
    let projection = dispatch.projection();
    ContextAccounting {
        accounting_version: CONTEXT_ACCOUNTING_VERSION,
        turn_index,
        attempt,
        estimator: projection.estimator.to_string(),
        components: projection.components.clone(),
        estimated_input_tokens: dispatch.context().tokens,
        raw_estimated_input_tokens: Some(projection.estimated_input_tokens),
        input_calibration_basis_points: Some(dispatch.context().ratio_basis_points),
        anchored_input_tokens: dispatch.context().anchored,
        context_window: config.context_window,
        compaction_threshold_basis_points: crate::provider_input::budget::threshold_basis_points(
            config.compaction_threshold,
        ),
        compaction_threshold_tokens: crate::provider_input::budget::compaction_input_budget(
            config.context_window,
            config.compaction_threshold,
            crate::provider_input::budget::configured_output_ceiling(config.max_tokens),
        ),
        configured_max_output_tokens: config.max_tokens,
        effective_max_output_tokens: request.max_tokens,
        compaction_reason,
        pre_compaction_input_tokens,
    }
}

pub fn clamp_request_output_budget(
    request: &mut CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) {
    let configured_max =
        crate::provider_input::budget::configured_output_ceiling(request.max_tokens);
    let effective_max = crate::provider_input::budget::effective_output_budget(
        input_tokens,
        config.context_window,
        configured_max,
    );
    if effective_max < configured_max {
        tracing::debug!(
            target: "gents::agent::loop_stream",
            input_tokens,
            context_window = config.context_window,
            configured_max_output_tokens = configured_max,
            effective_max_output_tokens = effective_max,
            "clamped completion output to remaining provider context"
        );
    }
    request.max_tokens = Some(
        u64::try_from(effective_max)
            .expect("a usize provider output ceiling is representable as u64"),
    );
}

pub fn ensure_context_can_dispatch(
    request: &CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) -> Result<(), StreamingError> {
    let configured_max =
        crate::provider_input::budget::configured_output_ceiling(request.max_tokens);
    if crate::provider_input::budget::can_dispatch(
        input_tokens,
        config.context_window,
        configured_max,
    ) {
        return Ok(());
    }
    Err(StreamingError::Completion(CompletionError::RequestError(
        Box::new(
            crate::provider_input::budget::ContextBudgetError::NoOutputCapacity {
                estimated_input_tokens: input_tokens,
                context_window: config.context_window,
                effective_max_output_tokens: configured_max,
            },
        ),
    )))
}
