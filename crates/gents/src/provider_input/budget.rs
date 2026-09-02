//! Provider-input budget policy.
//!
//! This is the Rust owner of `PromptAssembly.Budget`. Compaction consults this
//! policy, but does not own it: the same exact arithmetic gates every provider
//! dispatch whether or not reduction is available.

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContextBudgetError {
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

/// A fractional threshold as integer basis points.
///
/// The configuration surface carries the threshold as `f64`, but every budget
/// decision after this conversion uses exact integer arithmetic. Rounding
/// recovers percentage/basis-point configuration values such as 57%, whose
/// binary floating-point representation lies just below the exact value.
pub(crate) fn threshold_basis_points(threshold: f64) -> u64 {
    if !threshold.is_finite() || threshold <= 0.0 {
        return 0;
    }
    (threshold * 10_000.0).round().min(10_000.0) as u64
}

/// Exact configured input threshold for a context window.
pub fn threshold_budget(context_window: usize, threshold: f64) -> usize {
    let basis_points = u128::from(threshold_basis_points(threshold));
    ((context_window as u128 * basis_points) / 10_000) as usize
}

/// Provider input available before reduction. Output remains a dynamic
/// per-dispatch ceiling rather than an up-front reservation.
pub fn effective_input_budget(context_window: usize, threshold: f64) -> usize {
    threshold_budget(context_window, threshold).min(context_window)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThresholdDecision {
    NotNeeded,
    ReduceEligible,
}

/// The single equality-sensitive provider-input threshold decision.
pub(crate) fn threshold_decision(
    input_tokens: usize,
    effective_input_budget: usize,
) -> ThresholdDecision {
    if input_tokens <= effective_input_budget {
        ThresholdDecision::NotNeeded
    } else {
        ThresholdDecision::ReduceEligible
    }
}

/// Preserve one interpretation of an optional provider output ceiling across
/// pointer widths. `None` retains the provider-unbounded configuration meaning;
/// dispatch preparation always replaces it with an explicit dynamic ceiling.
pub(crate) fn configured_output_ceiling(max_tokens: Option<u64>) -> usize {
    max_tokens
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX)
}

/// Pair-safe recent-history target after fixed provider layers. One quarter of
/// the remaining input capacity is reserved for the checkpoint and framing.
pub(crate) fn compaction_retention_target(
    configured_keep_recent: usize,
    effective_input_budget: usize,
    fixed_input_tokens: usize,
) -> usize {
    let message_budget = effective_input_budget.saturating_sub(fixed_input_tokens);
    configured_keep_recent.min(((message_budget as u128 * 3) / 4) as usize)
}

/// Bound an internal summary's configured ceiling to one rounded-up quarter of
/// its actual context window.
pub(crate) fn summary_output_ceiling(
    configured_max_output_tokens: usize,
    context_window: usize,
) -> usize {
    configured_max_output_tokens.min(context_window.div_ceil(4).max(1))
}

/// Maximum rolling-summary input that preserves the configured summary output
/// whenever that ceiling can coexist with non-empty input.
pub(crate) fn rolling_summary_input_budget(
    context_window: usize,
    configured_max_output_tokens: usize,
) -> usize {
    if configured_max_output_tokens < context_window {
        context_window - configured_max_output_tokens
    } else {
        context_window
    }
}

/// Dynamic output allowance after the complete provider-shaped input is known.
pub(crate) fn effective_output_budget(
    input_tokens: usize,
    context_window: usize,
    configured_max_output_tokens: usize,
) -> usize {
    configured_max_output_tokens.min(context_window.saturating_sub(input_tokens))
}

/// A request is dispatchable only with strictly positive output capacity and
/// an overflow-safe total that fits the context.
pub(crate) fn can_dispatch(
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
