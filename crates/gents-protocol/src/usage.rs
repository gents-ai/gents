use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    #[error(transparent)]
    Counter(#[from] std::num::TryFromIntError),
    #[error("complete usage requires both input and output counters")]
    IncompleteCounters,
    #[error("{name} token counter exceeds the durable range")]
    CounterRange { name: &'static str },
    #[error("{name} exceeds its containing token counter")]
    PartExceedsWhole { name: &'static str },
    #[error("cache read and write exceed input tokens")]
    CacheExceedsInput,
    #[error("token usage summary exceeds the durable range")]
    SummaryRange,
}

type Result<T, E = UsageError> = std::result::Result<T, E>;

pub const TOKEN_COUNTER_LIMIT: u64 = i64::MAX as u64;

/// Input includes cache reads and writes; reasoning is part of output.
/// A missing counter remains unreported even when another counter is zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub provider_total_tokens: Option<u64>,
    pub final_response: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCompleteness {
    Missing,
    Partial,
    Complete,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsageColumns {
    #[serde(default)]
    pub prompt_tokens: Option<i64>,
    #[serde(default)]
    pub completion_tokens: Option<i64>,
    #[serde(default)]
    pub cached_input_tokens: Option<i64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    pub reasoning_output_tokens: Option<i64>,
    #[serde(default)]
    pub provider_total_tokens: Option<i64>,
    #[serde(default)]
    pub usage_completeness: Option<UsageCompleteness>,
}

impl TokenUsageColumns {
    pub fn observation(&self) -> Result<TokenUsage> {
        let counter = |value: Option<i64>| value.map(u64::try_from).transpose();
        let observation = TokenUsage {
            input_tokens: counter(self.prompt_tokens)?,
            output_tokens: counter(self.completion_tokens)?,
            cached_input_tokens: counter(self.cached_input_tokens)?,
            cache_creation_input_tokens: counter(self.cache_creation_input_tokens)?,
            reasoning_output_tokens: counter(self.reasoning_output_tokens)?,
            provider_total_tokens: counter(self.provider_total_tokens)?,
            final_response: self.usage_completeness == Some(UsageCompleteness::Complete),
        };
        observation.validate()?;
        if self.usage_completeness == Some(UsageCompleteness::Complete)
            && observation.completeness() != UsageCompleteness::Complete
        {
            return Err(UsageError::IncompleteCounters);
        }
        Ok(observation)
    }
}

impl TokenUsage {
    /// Normalize providers that report visible output separately from reasoning.
    pub fn with_separate_reasoning(mut self) -> Self {
        self.output_tokens = self
            .output_tokens
            .zip(self.reasoning_output_tokens)
            .and_then(|(output, reasoning)| output.checked_add(reasoning));
        self
    }

    /// Retain bounded core counters and discard contradictory breakdowns so
    /// every stored observation remains readable without establishing a charge.
    pub fn for_storage(self) -> Self {
        let bounded = |value: Option<u64>| value.filter(|value| *value <= TOKEN_COUNTER_LIMIT);
        let mut stored = Self {
            input_tokens: bounded(self.input_tokens),
            output_tokens: bounded(self.output_tokens),
            cached_input_tokens: bounded(self.cached_input_tokens),
            cache_creation_input_tokens: bounded(self.cache_creation_input_tokens),
            reasoning_output_tokens: bounded(self.reasoning_output_tokens),
            provider_total_tokens: bounded(self.provider_total_tokens),
            final_response: self.final_response && self.validate().is_ok(),
        };
        if stored.validate().is_err() {
            stored.cached_input_tokens = None;
            stored.cache_creation_input_tokens = None;
            stored.reasoning_output_tokens = None;
        }
        stored
    }

    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("input", self.input_tokens),
            ("output", self.output_tokens),
            ("cache read", self.cached_input_tokens),
            ("cache write", self.cache_creation_input_tokens),
            ("reasoning", self.reasoning_output_tokens),
            ("provider total", self.provider_total_tokens),
        ] {
            if value.is_some_and(|value| value > TOKEN_COUNTER_LIMIT) {
                return Err(UsageError::CounterRange { name });
            }
        }
        for (name, part, whole) in [
            ("cache read", self.cached_input_tokens, self.input_tokens),
            (
                "cache write",
                self.cache_creation_input_tokens,
                self.input_tokens,
            ),
            (
                "reasoning",
                self.reasoning_output_tokens,
                self.output_tokens,
            ),
        ] {
            if let (Some(part), Some(whole)) = (part, whole) {
                if part > whole {
                    return Err(UsageError::PartExceedsWhole { name });
                }
            }
        }
        if let (Some(input), Some(read), Some(write)) = (
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_creation_input_tokens,
        ) {
            if read + write > input {
                return Err(UsageError::CacheExceedsInput);
            }
        }
        Ok(())
    }

    pub fn completeness(&self) -> UsageCompleteness {
        let has_counters = [
            self.input_tokens,
            self.output_tokens,
            self.cached_input_tokens,
            self.cache_creation_input_tokens,
            self.reasoning_output_tokens,
            self.provider_total_tokens,
        ]
        .iter()
        .any(Option::is_some);
        if !has_counters {
            UsageCompleteness::Missing
        } else if self.final_response && self.input_tokens.is_some() && self.output_tokens.is_some()
        {
            UsageCompleteness::Complete
        } else {
            UsageCompleteness::Partial
        }
    }

    /// The provider's total is retained as evidence, never a second charge.
    pub fn charged_tokens(&self) -> Result<Option<u64>> {
        self.validate()?;
        Ok(
            match (self.completeness(), self.input_tokens, self.output_tokens) {
                (UsageCompleteness::Complete, Some(input), Some(output)) => Some(input + output),
                _ => None,
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenColumn {
    pub known: u64,
    pub reported: u64,
}

impl TokenColumn {
    pub fn observed(&self) -> Option<u64> {
        (self.reported > 0).then_some(self.known)
    }

    fn add(&mut self, value: Option<u64>) -> Result<()> {
        if let Some(value) = value {
            self.known = add_counter(self.known, value)?;
            self.reported = add_counter(self.reported, 1)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsageSummary {
    pub calls: u64,
    pub incomplete_calls: u64,
    pub input: TokenColumn,
    pub output: TokenColumn,
    pub cache_read: TokenColumn,
    pub cache_write: TokenColumn,
    pub reasoning: TokenColumn,
    pub provider_total: TokenColumn,
}

impl TokenUsageSummary {
    pub fn charged_total(&self) -> Option<u64> {
        (self.incomplete_calls == 0)
            .then(|| self.input.known.checked_add(self.output.known))
            .flatten()
    }

    pub fn fresh_input(&self) -> Option<u64> {
        (self.input.reported == self.calls && self.cache_read.reported == self.calls)
            .then(|| self.input.known.saturating_sub(self.cache_read.known))
    }

    pub fn from_observations<I>(observations: I) -> Result<Self>
    where
        I: IntoIterator<Item = TokenUsage>,
    {
        let mut summary = Self::default();
        for usage in observations {
            usage.validate()?;
            summary.calls = add_counter(summary.calls, 1)?;
            if usage.completeness() != UsageCompleteness::Complete {
                summary.incomplete_calls = add_counter(summary.incomplete_calls, 1)?;
            }
            summary.input.add(usage.input_tokens)?;
            summary.output.add(usage.output_tokens)?;
            summary.cache_read.add(usage.cached_input_tokens)?;
            summary.cache_write.add(usage.cache_creation_input_tokens)?;
            summary.reasoning.add(usage.reasoning_output_tokens)?;
            summary.provider_total.add(usage.provider_total_tokens)?;
        }
        Ok(summary)
    }
}

fn add_counter(left: u64, right: u64) -> Result<u64> {
    let sum = left
        .checked_add(right)
        .filter(|sum| *sum <= TOKEN_COUNTER_LIMIT);
    sum.ok_or(UsageError::SummaryRange)
}
