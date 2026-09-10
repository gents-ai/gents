use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Owned-loop execution settings. These are configuration limits; active request
/// deadlines, retry counters, and token ledgers remain owned execution state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceExecution {
    pub agent_did: String,
    pub execution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_turns: Option<i64>,
    /// Optional aggregate provider-token limit for one physical request, shared by
    /// its inference and compaction calls. Positive when set; unset is unlimited.
    /// The execution owner pins the resolved limit and rehydrates usage on restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_total_tokens: Option<i64>,
    /// Persistence batching cadence, not provider latency. Existing default 1,000ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub stream_batch_ms: Option<i64>,
    /// Maximum provider-stream silence. Existing default 1,800s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub stream_liveness_timeout_secs: Option<i64>,
    /// Overall claimed-request duration. Existing default 86,400s; includes tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub deadline_duration_secs: Option<i64>,
    /// Unset uses the existing retry policy for the request's execution origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub retry_policy_id: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Retry policy selected by InferenceExecution. Unset fields preserve the existing
/// owned-loop defaults and classification; retries share the request deadline/budget.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceRetryPolicy {
    pub agent_did: String,
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
    /// Default interactive successor ceiling, resolved onto the initial request.
    /// Its retry chain retains that ceiling; this is not a human approval gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub interactive_max_retries: Option<i64>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl InferenceExecution {
    /// Execution-owned validation, the single owner every write path calls.
    /// Authored `max_total_tokens` must be positive when set; unset is
    /// unlimited. A restarted request's runtime-pinned exhausted `Some(0)`
    /// ledger is owned execution state and is never validated through this
    /// document path, so the positivity rule never defaults it away.
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
            ("deadline_duration_secs", self.deadline_duration_secs),
        ] {
            if value.is_some_and(|value| value <= 0) {
                violations.push(format!(
                    "InferenceExecution {execution_id} {name} must be positive"
                ));
            }
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

    pub fn validate(&self) -> Result<()> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(violations.join("; "))
        }
    }
}

impl InferenceRetryPolicy {
    /// Retry-policy-owned validation. Explicit retry counts must be
    /// non-negative; zero is a legitimate explicit "no retries" ceiling.
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

    pub fn validate(&self) -> Result<()> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(violations.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_validation_uses_effective_defaults_for_cross_field_limits() {
        assert!(InferenceExecution::default().validate().is_ok());
        for execution in [
            InferenceExecution {
                deadline_duration_secs: Some(
                    crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64,
                ),
                ..Default::default()
            },
            InferenceExecution {
                stream_liveness_timeout_secs: Some(
                    crate::config::DEFAULT_DEADLINE_DURATION_SECS as i64,
                ),
                ..Default::default()
            },
            InferenceExecution {
                stream_liveness_timeout_secs: Some(600),
                deadline_duration_secs: Some(300),
                ..Default::default()
            },
        ] {
            assert!(execution.validate().is_err());
        }
        assert!(InferenceExecution {
            stream_liveness_timeout_secs: Some(1),
            deadline_duration_secs: Some(2),
            ..Default::default()
        }
        .validate()
        .is_ok());
        let execution = InferenceExecution {
            max_turns: Some(0),
            max_total_tokens: Some(0),
            stream_batch_ms: Some(0),
            ..Default::default()
        };
        assert_eq!(execution.validation_violations().len(), 3);
    }

    #[test]
    fn retry_zero_means_no_retries_but_invalid_delays_are_not_silently_dropped() {
        let mut retry = InferenceRetryPolicy {
            max_transport_retries: Some(0),
            max_resample_retries: Some(0),
            interactive_max_retries: Some(0),
            backoff_ms: Some(vec![]),
            ..Default::default()
        };
        assert!(retry.validate().is_ok());
        retry.backoff_ms = Some(vec![1, 1000]);
        assert!(retry.validate().is_ok());
        for delays in [vec![0], vec![-1], vec![1, 0, 1000]] {
            retry.backoff_ms = Some(delays);
            assert!(retry.validate().is_err());
        }
        retry.backoff_ms = None;
        retry.max_transport_retries = Some(-1);
        assert!(retry.validate().is_err());
    }
}
