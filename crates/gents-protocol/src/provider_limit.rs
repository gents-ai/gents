use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The provider-limit verdict a failed InferenceCall records in
/// `provider_limit`, in the terminal write that records its `failure_reason`
/// (Lean: `InferenceCall.ProviderLimit.Verdict`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLimitVerdict {
    Throttled,
    UsageLimited,
}

impl ProviderLimitVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Throttled => "throttled",
            Self::UsageLimited => "usage_limited",
        }
    }
}

/// What a usage refusal states about the limit it hit, each part only when
/// the provider states it: recorded in `provider_limit_window_minutes`,
/// `provider_limit_tokens_used` and `provider_limit_tokens_limit`
/// (Lean: `InferenceCall.ProviderLimit.Statement`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderLimitStatement {
    pub window_minutes: Option<u64>,
    pub tokens_used: Option<u64>,
    pub tokens_limit: Option<u64>,
}

/// A failed call's `provider_limit` and, for a usage limit, the reset the
/// provider reported in `provider_limit_resets_at` and what it stated of the
/// limit (Lean: `InferenceCall.ProviderLimit.Recorded`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedProviderLimit {
    pub verdict: ProviderLimitVerdict,
    pub resets_at: Option<DateTime<Utc>>,
    pub statement: ProviderLimitStatement,
}
