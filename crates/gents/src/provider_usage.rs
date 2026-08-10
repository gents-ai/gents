//! Shared provider-usage charge semantics.
//!
//! The request-wide aggregate ledger ([`crate::agent::loop_stream`]) and the
//! durable `InferenceCall` write path ([`crate::admission::persistence`]) must
//! charge the same way: otherwise Harbor/ATIF sums of persisted rows diverge
//! from the live ledger, and a crash-rehydrate path would disagree with either.
//!
//! Mirrors `PromptAssembly.AggregateBudget.Usage.chargedTotal` in Lean: take
//! the larger of the provider's reported total and the sum of its
//! input/output components so an inconsistent report can never undercharge.

use rig::completion::Usage;

/// Tokens charged against a request-wide budget for one provider usage report.
///
/// Equal to `max(total_tokens, input_tokens + output_tokens)`. Zero means the
/// report is not enforceable (treated as missing by the ledger).
pub(crate) fn charged_usage_total(usage: Usage) -> u64 {
    usage
        .total_tokens
        .max(usage.input_tokens.saturating_add(usage.output_tokens))
}

/// Persist token columns with Harbor/ATIF semantics so
/// `prompt_tokens + completion_tokens == charged_usage_total(usage)`.
///
/// - `prompt_tokens` is every input token implied by the charged total
///   (including cache components that only appear in `total_tokens`)
/// - `completion_tokens` is reported output
/// - `cached_input_tokens` is the reported cache-read count (metadata only;
///   already folded into the charged total when the provider put it in
///   `total_tokens`)
pub(crate) fn persisted_usage_counts(usage: Usage) -> (u64, u64, u64) {
    let charged_total = charged_usage_total(usage);
    (
        charged_total.saturating_sub(usage.output_tokens),
        usage.output_tokens,
        usage.cached_input_tokens,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::completion::Usage;

    #[test]
    fn charged_total_covers_components_and_cannot_underreport() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 200,
            cached_input_tokens: 40,
            cache_creation_input_tokens: 10,
        };
        assert_eq!(charged_usage_total(usage), 200);
        assert_eq!(persisted_usage_counts(usage), (150, 50, 40));

        let inconsistent = Usage {
            input_tokens: 300,
            output_tokens: 200,
            total_tokens: 400,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        assert_eq!(charged_usage_total(inconsistent), 500);
        assert_eq!(persisted_usage_counts(inconsistent), (300, 200, 0));
    }

    #[test]
    fn persisted_parts_sum_to_charged_total() {
        for usage in [
            Usage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 200,
                cached_input_tokens: 40,
                cache_creation_input_tokens: 10,
            },
            Usage {
                input_tokens: 300,
                output_tokens: 200,
                total_tokens: 400,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            Usage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        ] {
            let charged = charged_usage_total(usage);
            let (prompt, completion, _cached) = persisted_usage_counts(usage);
            assert_eq!(
                prompt.saturating_add(completion),
                charged,
                "durable columns must reconstruct the ledger charge: {usage:?}"
            );
        }
    }
}
