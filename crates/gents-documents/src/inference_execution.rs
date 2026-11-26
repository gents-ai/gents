use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Owned-loop execution settings; these are configuration limits only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
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
    /// Invalid-argument, unknown-tool and policy-denied calls one execution
    /// may make before it stops; default 8.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_invalid_tool_calls: Option<i64>,
    /// Identical failing calls (same tool, arguments and error) dispatched
    /// before the next repeat is refused; default 3.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub repeated_tool_failure_limit: Option<i64>,
    /// Successors (operator reissues, background wake redrives) one request
    /// lineage may take; the first claim pins it on the request, so a change
    /// reaches only lineages claimed after it. Zero allows none; default 3.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_request_retries: Option<i64>,
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
    /// Title-generation attempt timeout; default 10s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub title_timeout_secs: Option<i64>,
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
    #[schemars(transform = crate::accepts_null)]
    pub tags: Vec<String>,
}

/// Retry policy selected by InferenceExecution; unset fields preserve the
/// owned-loop defaults and classification.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
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
    /// Provider transport retries for interactive requests, in place of
    /// `max_transport_retries`; not a successor-request ceiling (that is
    /// `InferenceExecution.max_request_retries`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub interactive_max_retries: Option<i64>,
    /// Longest provider rate-limit wait the owned loop holds; a longer wait
    /// fails the request as the provider's usage limit, and a failed call
    /// records that verdict. Default 300,000ms; `backoff_ms` steps may not
    /// exceed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_rate_limit_wait_ms: Option<i64>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub tags: Vec<String>,
}

/// Largest admitted `deadline_duration_secs`: 100 years. A claim computes
/// `now + deadline` as an RFC 3339 timestamp; the bound keeps that sum inside
/// four-digit years.
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
            ("title_timeout_secs", self.title_timeout_secs),
            ("deadline_duration_secs", self.deadline_duration_secs),
        ] {
            if value.is_some_and(|value| value <= 0) {
                violations.push(format!(
                    "InferenceExecution {execution_id} {name} must be positive"
                ));
            }
        }
        for (name, limit) in [
            ("max_invalid_tool_calls", self.invalid_tool_call_allowance()),
            ("repeated_tool_failure_limit", self.repeated_failure_limit()),
        ] {
            if limit.is_none() {
                violations.push(format!(
                    "InferenceExecution {execution_id} {name} must be positive"
                ));
            }
        }
        if let Err(error) = self.request_retry_budget() {
            violations.push(format!("InferenceExecution {execution_id} {error}"));
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
            .unwrap_or(crate::inference_defaults::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64);
        let deadline = self
            .deadline_duration_secs
            .unwrap_or(crate::inference_defaults::DEFAULT_DEADLINE_DURATION_SECS as i64);
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

    /// The owned loop's invalid-tool-call allowance; `None` is refused.
    pub fn invalid_tool_call_allowance(&self) -> Option<usize> {
        configured_loop_limit(
            self.max_invalid_tool_calls,
            gents_loop::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        )
    }

    /// The successor budget a request's first claim pins (Lean
    /// `Request.PinnedRetryBudget.resolveConfigured`): unset takes
    /// `DEFAULT_REQUEST_MAX_RETRIES`, zero allows no successors, and a
    /// negative value is refused.
    pub fn request_retry_budget(&self) -> Result<u64, RequestRetryBudgetError> {
        match self.max_request_retries {
            None => Ok(crate::inference_defaults::DEFAULT_REQUEST_MAX_RETRIES),
            Some(value) => u64::try_from(value).map_err(|_| RequestRetryBudgetError::Negative),
        }
    }

    /// The owned loop's identical-failure limit; `None` is refused.
    pub fn repeated_failure_limit(&self) -> Option<usize> {
        configured_loop_limit(
            self.repeated_tool_failure_limit,
            gents_loop::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestRetryBudgetError {
    #[error("max_request_retries must be non-negative")]
    Negative,
}

/// A loop-guard limit as configured (Lean `InvalidToolProgress.configuredLimit`):
/// unset takes `default`, a positive value is used as is, and any other value
/// is refused.
pub fn configured_loop_limit(configured: Option<i64>, default: usize) -> Option<usize> {
    match configured {
        None => Some(default),
        Some(value) if value > 0 => usize::try_from(value).ok(),
        Some(_) => None,
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

        if self.max_rate_limit_wait_ms.is_some_and(|value| value <= 0) {
            violations.push(format!(
                "InferenceRetryPolicy {retry_policy_id} max_rate_limit_wait_ms must be positive"
            ));
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
        let max_wait_ms = self
            .completion_retry_fields()
            .max_rate_limit_wait()
            .as_millis();
        if self.backoff_ms.as_ref().is_some_and(|values| {
            values
                .iter()
                .any(|value| u128::try_from(*value).is_ok_and(|value| value > max_wait_ms))
        }) {
            violations.push(format!(
                "InferenceRetryPolicy {retry_policy_id} backoff_ms entries must not exceed max_rate_limit_wait_ms ({max_wait_ms})"
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

    /// The owned loop's view of this policy; unset fields keep the loop's
    /// origin defaults.
    pub fn completion_retry_fields(
        &self,
    ) -> gents_loop::completion_retry::CompletionRetryProfileFields {
        gents_loop::completion_retry::CompletionRetryProfileFields {
            retry_max_transport: self.max_transport_retries,
            retry_backoff_ms: self.backoff_ms.clone(),
            retry_max_resample: self.max_resample_retries,
            retry_allow_repair: self.allow_repair,
            retry_interactive_max: self.interactive_max_retries,
            retry_max_rate_limit_wait_ms: self.max_rate_limit_wait_ms,
        }
    }
}

#[cfg(test)]
mod configured_limit_tests;
#[cfg(test)]
mod pushdown_tests;
