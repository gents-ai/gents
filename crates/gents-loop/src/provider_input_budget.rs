#[derive(Debug, thiserror::Error)]
pub enum ContextBudgetError {
    #[error(
        "provider_input_has_no_output_capacity: estimated_input_tokens={estimated_input_tokens}, \
         context_window={context_window}, effective_max_output_tokens={effective_max_output_tokens}"
    )]
    NoOutputCapacity {
        estimated_input_tokens: usize,
        context_window: usize,
        effective_max_output_tokens: usize,
    },
}

pub fn threshold_basis_points(threshold: f64) -> u64 {
    if !threshold.is_finite() || threshold <= 0.0 {
        return 0;
    }
    (threshold * 10_000.0).round().min(10_000.0) as u64
}

pub fn threshold_budget(context_window: usize, threshold: f64) -> usize {
    let basis_points = u128::from(threshold_basis_points(threshold));
    ((context_window as u128 * basis_points) / 10_000) as usize
}

pub fn effective_input_budget(context_window: usize, threshold: f64) -> usize {
    threshold_budget(context_window, threshold).min(context_window)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThresholdDecision {
    NotNeeded,
    ReduceEligible,
}

pub fn threshold_decision(input_tokens: usize, effective_input_budget: usize) -> ThresholdDecision {
    if input_tokens <= effective_input_budget {
        ThresholdDecision::NotNeeded
    } else {
        ThresholdDecision::ReduceEligible
    }
}

pub fn configured_output_ceiling(max_tokens: Option<u64>) -> usize {
    max_tokens
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX)
}

pub fn compaction_retention_target(
    configured_keep_recent: usize,
    effective_input_budget: usize,
    fixed_input_tokens: usize,
) -> usize {
    let message_budget = effective_input_budget.saturating_sub(fixed_input_tokens);
    configured_keep_recent.min(((message_budget as u128 * 3) / 4) as usize)
}

pub fn summary_output_ceiling(configured_max_output_tokens: usize, context_window: usize) -> usize {
    configured_max_output_tokens.min(context_window.div_ceil(4).max(1))
}

pub fn rolling_summary_input_budget(
    context_window: usize,
    configured_max_output_tokens: usize,
) -> usize {
    if configured_max_output_tokens < context_window {
        context_window - configured_max_output_tokens
    } else {
        context_window
    }
}

pub fn effective_output_budget(
    input_tokens: usize,
    context_window: usize,
    configured_max_output_tokens: usize,
) -> usize {
    configured_max_output_tokens.min(context_window.saturating_sub(input_tokens))
}

pub fn can_dispatch(
    input_tokens: usize,
    context_window: usize,
    configured_max_output_tokens: usize,
) -> bool {
    let output_tokens =
        effective_output_budget(input_tokens, context_window, configured_max_output_tokens);
    output_tokens > 0
        && input_tokens
            .checked_add(output_tokens)
            .is_some_and(|total| total <= context_window)
}
