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

/// A failed call's `provider_limit` and, for a usage limit the provider
/// reported a reset for, its `provider_limit_resets_at`
/// (Lean: `InferenceCall.ProviderLimit.Recorded`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedProviderLimit {
    pub verdict: ProviderLimitVerdict,
    pub resets_at: Option<DateTime<Utc>>,
}
