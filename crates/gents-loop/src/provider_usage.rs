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
