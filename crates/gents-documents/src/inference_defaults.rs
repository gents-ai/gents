pub const DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_DEADLINE_DURATION_SECS: u64 = 86_400;
/// Unset `InferenceExecution.max_request_retries`: the successor budget a
/// request's first claim pins (Lean
/// `Request.PinnedRetryBudget.defaultMaxRequestRetries`). Provider transport
/// retries are `InferenceRetryPolicy`'s.
pub const DEFAULT_REQUEST_MAX_RETRIES: u64 = 3;
pub const DEFAULT_MAX_CONCURRENT: i64 = 1;
pub const DEFAULT_MAX_QUEUE_DEPTH: i64 = 100;
pub const DEFAULT_CONNECT_TIMEOUT_SECS: i64 = 10;
pub const DEFAULT_DISCOVERY_TIMEOUT_SECS: i64 = 10;
/// Unset `request_timeout_secs` default. Bounds one host-side model exchange
/// independently of the guest's remaining wall-clock budget, so a stalled
/// endpoint cannot occupy its permit for the entire guest lifetime.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: i64 = 120;
pub const ANTHROPIC_API_ENDPOINT: &str = "https://api.anthropic.com/v1";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    pub const ALL: [Self; 8] = [
        Self::None,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
        Self::Ultra,
    ];

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            "ultra" => Ok(Self::Ultra),
            _ => anyhow::bail!(
                "reasoning_effort must be one of: none, minimal, low, medium, high, xhigh, max, ultra"
            ),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::ReasoningEffort;

    #[test]
    fn reasoning_efforts_are_the_lean_reasoning_efforts() {
        gents_lean_vocab::families::configuration_reasoning_effort_vocabulary::replay(
            &ReasoningEffort::ALL,
        );
    }
}
