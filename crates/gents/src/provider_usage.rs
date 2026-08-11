//! Shared provider-usage charge semantics.
//!
//! The request-wide aggregate ledger ([`crate::agent::loop_stream`]) and the
//! durable `InferenceCall` write path ([`crate::admission::persistence`]) must
//! charge the same way: otherwise Harbor/ATIF sums of persisted rows diverge
//! from the live ledger, and a crash-rehydrate path would disagree with either.
//!
//! Mirrors `PromptAssembly.AggregateBudget.Usage.chargedTotal` in Lean: charge
//! the provider input/output components already stored on `InferenceCall`.
//! `total_tokens` remains an observed provider value, not a second accounting
//! source that has no durable column.

use rig::completion::Usage;

/// Tokens charged against a request-wide budget for one provider usage report.
///
/// Equal to `input_tokens + output_tokens`, the durable components used by
/// restart rehydration. Zero means the report is not enforceable (treated as
/// missing by the ledger).
pub(crate) fn charged_usage_total(usage: Usage) -> u64 {
    usage.input_tokens.saturating_add(usage.output_tokens)
}

/// Reconstruct the charged total from durable `InferenceCall` columns.
///
/// Persistence writes both provider components together. Both absent means the
/// call never reported usage; any partial or negative row is corrupt and must
/// fail budget rehydration closed.
fn charged_from_persisted_parts(
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
) -> anyhow::Result<Option<u64>> {
    match (prompt_tokens, completion_tokens) {
        (None, None) => Ok(None),
        (Some(prompt), Some(completion)) => {
            let prompt = u64::try_from(prompt)
                .map_err(|_| anyhow::anyhow!("persisted prompt_tokens must be non-negative"))?;
            let completion = u64::try_from(completion)
                .map_err(|_| anyhow::anyhow!("persisted completion_tokens must be non-negative"))?;
            Ok(Some(prompt.saturating_add(completion)))
        }
        _ => anyhow::bail!(
            "persisted prompt_tokens and completion_tokens must both be present or both be absent"
        ),
    }
}

/// Sum charged totals across durable usage rows for one physical request.
pub(crate) fn sum_charged_from_persisted_parts<I>(rows: I) -> anyhow::Result<u64>
where
    I: IntoIterator<Item = (Option<i64>, Option<i64>)>,
{
    rows.into_iter()
        .try_fold(0u64, |total, (prompt, completion)| {
            Ok(total.saturating_add(
                charged_from_persisted_parts(prompt, completion)?.unwrap_or_default(),
            ))
        })
}

/// Sum durable usage columns across rows for observation (ATIF / Harbor).
///
/// Each column stays `None` until at least one row supplies a non-negative
/// value for it — the same rule the ledger's rehydrate path uses per field.
/// Callers that need the request-wide charged total should use
/// [`sum_charged_from_persisted_parts`] over the raw rows.
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
    fn charged_total_is_the_durable_input_output_sum() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 200,
            cached_input_tokens: 40,
            cache_creation_input_tokens: 10,
        };
        assert_eq!(charged_usage_total(usage), 150);

        let inconsistent = Usage {
            input_tokens: 300,
            output_tokens: 200,
            total_tokens: 400,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        assert_eq!(charged_usage_total(inconsistent), 500);
    }

    #[test]
    fn provider_total_does_not_rewrite_durable_prompt_tokens() {
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
            let prompt = usage.input_tokens;
            let completion = usage.output_tokens;
            assert_eq!(
                prompt.saturating_add(completion),
                charged,
                "durable columns must reconstruct the ledger charge: {usage:?}"
            );
            assert_eq!(
                charged_from_persisted_parts(Some(prompt as i64), Some(completion as i64)).unwrap(),
                Some(charged),
                "rehydrate inverse of persist: {usage:?}"
            );
        }
    }

    #[test]
    fn rehydrate_skips_rows_without_usage_and_sums_complete_rows() {
        assert_eq!(charged_from_persisted_parts(None, None).unwrap(), None);
        assert_eq!(
            sum_charged_from_persisted_parts([
                (None, None),
                (Some(100), Some(50)),
                (Some(200), Some(10)),
            ])
            .unwrap(),
            360
        );
    }

    #[test]
    fn rehydrate_rejects_partial_and_negative_usage_rows() {
        for row in [
            (Some(100), None),
            (None, Some(50)),
            (Some(-1), Some(50)),
            (Some(100), Some(-1)),
        ] {
            assert!(
                charged_from_persisted_parts(row.0, row.1).is_err(),
                "invalid durable usage must fail closed: {row:?}"
            );
        }
    }

    #[test]
    fn column_totals_preserve_observed_provider_usage() {
        let rows = [
            (None, None, None),
            (Some(100), Some(50), Some(20)),
            (Some(200), Some(10), Some(5)),
        ];
        let (prompt, completion, cached) = sum_persisted_usage_columns(rows);
        assert_eq!(prompt, Some(300));
        assert_eq!(completion, Some(60));
        assert_eq!(cached, Some(25));
        assert_eq!(
            sum_charged_from_persisted_parts(rows.map(|(p, c, _)| (p, c))).unwrap(),
            360
        );
        assert_eq!(remaining_budget(1_000, 360), 640);
        assert_eq!(remaining_budget(300, 360), 0);
    }
}
