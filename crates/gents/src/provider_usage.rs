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

/// Reconstruct the charged total from durable `InferenceCall` columns.
///
/// [`persisted_usage_counts`] writes `prompt_tokens` and `completion_tokens`
/// so they sum to [`charged_usage_total`]. Rehydrate uses that inverse: when
/// either column is present the missing side is treated as zero (fail-closed
/// toward more spent, never less). Both absent means the call never reported
/// usage and contributes nothing.
pub(crate) fn charged_from_persisted_parts(
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
) -> Option<u64> {
    if prompt_tokens.is_none() && completion_tokens.is_none() {
        return None;
    }
    let prompt = nonnegative_tokens(prompt_tokens).unwrap_or(0);
    let completion = nonnegative_tokens(completion_tokens).unwrap_or(0);
    Some(prompt.saturating_add(completion))
}

/// Sum charged totals across durable usage rows for one physical request.
pub(crate) fn sum_charged_from_persisted_parts<I>(rows: I) -> u64
where
    I: IntoIterator<Item = (Option<i64>, Option<i64>)>,
{
    rows.into_iter()
        .filter_map(|(prompt, completion)| charged_from_persisted_parts(prompt, completion))
        .fold(0u64, u64::saturating_add)
}

/// Sum durable usage columns across rows for observation (ATIF / Harbor).
///
/// Each column stays `None` until at least one row supplies a non-negative
/// value for it — the same rule the ledger's rehydrate path uses per field.
/// Callers that need the request-wide charged total should use
/// [`charged_from_column_totals`] on the prompt/completion results (or
/// [`sum_charged_from_persisted_parts`] over the raw rows).
pub(crate) fn sum_persisted_usage_columns<I>(rows: I) -> (Option<u64>, Option<u64>, Option<u64>)
where
    I: IntoIterator<Item = (Option<i64>, Option<i64>, Option<i64>)>,
{
    let mut prompt = None::<u64>;
    let mut completion = None::<u64>;
    let mut cached = None::<u64>;
    for (prompt_tokens, completion_tokens, cached_input_tokens) in rows {
        add_column_total(&mut prompt, prompt_tokens);
        add_column_total(&mut completion, completion_tokens);
        add_column_total(&mut cached, cached_input_tokens);
    }
    (prompt, completion, cached)
}

/// Charged total from already-summed durable columns.
pub(crate) fn charged_from_column_totals(
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
) -> Option<u64> {
    if prompt_tokens.is_none() && completion_tokens.is_none() {
        return None;
    }
    Some(
        prompt_tokens
            .unwrap_or(0)
            .saturating_add(completion_tokens.unwrap_or(0)),
    )
}

/// Remaining request-wide budget after durable spend, when a ceiling is set.
pub(crate) fn remaining_budget(limit: u64, used: u64) -> u64 {
    limit.saturating_sub(used)
}

fn add_column_total(total: &mut Option<u64>, value: Option<i64>) {
    let Some(value) = nonnegative_tokens(value) else {
        return;
    };
    *total = Some(total.unwrap_or_default().saturating_add(value));
}

fn nonnegative_tokens(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
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
            assert_eq!(
                charged_from_persisted_parts(Some(prompt as i64), Some(completion as i64)),
                Some(charged),
                "rehydrate inverse of persist: {usage:?}"
            );
        }
    }

    #[test]
    fn rehydrate_skips_rows_without_usage_and_sums_the_rest() {
        assert_eq!(charged_from_persisted_parts(None, None), None);
        assert_eq!(charged_from_persisted_parts(Some(100), None), Some(100));
        assert_eq!(charged_from_persisted_parts(None, Some(50)), Some(50));
        assert_eq!(
            sum_charged_from_persisted_parts([
                (None, None),
                (Some(100), Some(50)),
                (Some(200), Some(10)),
            ]),
            360
        );
    }

    #[test]
    fn column_totals_and_charged_observation_agree_with_rehydrate() {
        let rows = [
            (None, None, None),
            (Some(100), Some(50), Some(20)),
            (Some(200), Some(10), Some(5)),
        ];
        let (prompt, completion, cached) = sum_persisted_usage_columns(rows);
        assert_eq!(prompt, Some(300));
        assert_eq!(completion, Some(60));
        assert_eq!(cached, Some(25));
        assert_eq!(charged_from_column_totals(prompt, completion), Some(360));
        assert_eq!(
            sum_charged_from_persisted_parts(rows.map(|(p, c, _)| (p, c))),
            360
        );
        assert_eq!(remaining_budget(1_000, 360), 640);
        assert_eq!(remaining_budget(300, 360), 0);
    }
}
