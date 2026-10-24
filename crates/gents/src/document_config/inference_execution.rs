use serde::{Deserialize, Serialize};

use super::error::DocumentConfigError;

/// Owned-loop execution settings; these are configuration limits only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceExecution {
    pub node_did: String,
    pub execution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_turns: Option<i64>,
    /// Optional per-request aggregate token limit; unset is unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_total_tokens: Option<i64>,
    /// Persistence batching cadence, not provider latency; default 1,000ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub stream_batch_ms: Option<i64>,
    /// Execution lease duration, renewed independently of provider output; default 120s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub stream_liveness_timeout_secs: Option<i64>,
    /// Max provider transport silence per attempt; expiry is retryable. Default 300s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub provider_idle_timeout_secs: Option<i64>,
    /// Overall claimed-request duration, including tools; default 86,400s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub deadline_duration_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub retry_policy_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Retry policy selected by InferenceExecution; unset fields preserve the
/// owned-loop defaults and classification.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceRetryPolicy {
    pub node_did: String,
    pub retry_policy_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_transport_retries: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub backoff_ms: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_resample_retries: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub allow_repair: Option<bool>,
    /// Default interactive successor ceiling; not a human approval gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub interactive_max_retries: Option<i64>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Largest admitted `deadline_duration_secs`: 100 years. A claim computes
/// `now + deadline` as an RFC 3339 timestamp; the bound keeps that sum inside
/// four-digit years (#2079).
pub const MAX_DEADLINE_DURATION_SECS: i64 = 100 * 365 * 24 * 60 * 60;

impl InferenceExecution {
    /// Execution-owned validation, the single owner every write path calls.
    /// The positivity rule never defaults away a runtime-pinned exhausted
    /// `Some(0)` ledger, which is owned execution state on a restarted request.
    pub fn validation_violations(&self) -> Vec<String> {
        let execution_id = self.execution_id.trim();
        let mut violations: Vec<String> = Vec::new();

        for (name, value) in [
            ("max_turns", self.max_turns),
            ("max_total_tokens", self.max_total_tokens),
            ("stream_batch_ms", self.stream_batch_ms),
            (
                "stream_liveness_timeout_secs",
                self.stream_liveness_timeout_secs,
            ),
            (
                "provider_idle_timeout_secs",
                self.provider_idle_timeout_secs,
            ),
            ("deadline_duration_secs", self.deadline_duration_secs),
        ] {
            if value.is_some_and(|value| value <= 0) {
                violations.push(format!(
                    "InferenceExecution {execution_id} {name} must be positive"
                ));
            }
        }
        if self
            .deadline_duration_secs
            .is_some_and(|value| value > MAX_DEADLINE_DURATION_SECS)
        {
            violations.push(format!(
                "InferenceExecution {execution_id} deadline_duration_secs must be at most {MAX_DEADLINE_DURATION_SECS} (100 years)"
            ));
        }
        let liveness = self
            .stream_liveness_timeout_secs
            .unwrap_or(crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64);
        let deadline = self
            .deadline_duration_secs
            .unwrap_or(crate::config::DEFAULT_DEADLINE_DURATION_SECS as i64);
        if liveness >= deadline {
            violations.push(format!("InferenceExecution {execution_id} stream_liveness_timeout_secs ({liveness}) must be less than deadline_duration_secs ({deadline})"));
        }

        violations
    }

    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(DocumentConfigError::InferenceExecutionViolations {
                violations: violations.join("; "),
            })
        }
    }
}

impl InferenceRetryPolicy {
    /// Retry-policy-owned validation; explicit retry counts must be
    /// non-negative, zero meaning "no retries".
    pub fn validation_violations(&self) -> Vec<String> {
        let retry_policy_id = self.retry_policy_id.trim();
        let mut violations: Vec<String> = Vec::new();

        for (name, value) in [
            ("max_transport_retries", self.max_transport_retries),
            ("max_resample_retries", self.max_resample_retries),
            ("interactive_max_retries", self.interactive_max_retries),
        ] {
            if value.is_some_and(|value| value < 0) {
                violations.push(format!(
                    "InferenceRetryPolicy {retry_policy_id} {name} must be non-negative"
                ));
            }
        }

        if self
            .backoff_ms
            .as_ref()
            .is_some_and(|values| values.iter().any(|value| *value <= 0))
        {
            violations.push(format!(
                "InferenceRetryPolicy {retry_policy_id} backoff_ms entries must be positive"
            ));
        }

        violations
    }

    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(DocumentConfigError::InferenceRetryPolicyViolations {
                violations: violations.join("; "),
            })
        }
    }
}
