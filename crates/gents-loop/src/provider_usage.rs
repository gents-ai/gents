use rig::completion::Usage;

#[derive(Debug, thiserror::Error)]
pub enum ProviderUsageError {
    #[error("persisted prompt_tokens must be non-negative")]
    NegativePromptTokens,
    #[error("persisted completion_tokens must be non-negative")]
    NegativeCompletionTokens,
    #[error(
        "persisted prompt_tokens and completion_tokens must both be present or both be absent"
    )]
    UnpairedTokens,
}

type Result<T, E = ProviderUsageError> = std::result::Result<T, E>;

pub fn charged_usage_total(usage: Usage) -> u64 {
    usage.input_tokens.saturating_add(usage.output_tokens)
}

/// The provider's whole count of one call's input under the usage contract
/// (input includes cache reads and writes). A report whose cache counters
/// exceed its input does not meet the contract, as rows written before Claude
/// usage was normalized do not, and yields nothing.
pub fn contract_input_tokens(input_tokens: u64, cache_read: u64, cache_write: u64) -> Option<u64> {
    (input_tokens > 0 && input_tokens >= cache_read.saturating_add(cache_write))
        .then_some(input_tokens)
}

/// [`contract_input_tokens`] of a rig usage report.
pub fn contract_usage_input_tokens(usage: &Usage) -> Option<u64> {
    contract_input_tokens(
        usage.input_tokens,
        usage.cached_input_tokens,
        usage.cache_creation_input_tokens,
    )
}

fn charged_from_persisted_parts(
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
) -> Result<Option<u64>> {
    match (prompt_tokens, completion_tokens) {
        (None, None) => Ok(None),
        (Some(prompt), Some(completion)) => {
            let prompt =
                u64::try_from(prompt).map_err(|_| ProviderUsageError::NegativePromptTokens)?;
            let completion = u64::try_from(completion)
                .map_err(|_| ProviderUsageError::NegativeCompletionTokens)?;
            Ok(Some(prompt.saturating_add(completion)))
        }
        _ => Err(ProviderUsageError::UnpairedTokens),
    }
}

pub fn sum_charged_from_persisted_parts<I>(rows: I) -> Result<u64>
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

pub fn sum_persisted_usage_columns<I>(rows: I) -> (Option<u64>, Option<u64>, Option<u64>)
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

pub fn remaining_budget(limit: u64, used: u64) -> u64 {
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
