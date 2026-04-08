//! Inference retry with exponential backoff and jitter.
//!
//! Wraps the streaming inference call with configurable retry behavior.
//! Only retries when the error is classified as transient (connection
//! failures, rate limits, timeouts) — permanent errors (auth, context
//! length) fail immediately.

use std::time::Duration;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::error::InferenceError;

/// Retry policy for inference calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retries).
    pub max_retries: u32,
    /// Base delay before first retry.
    pub base_delay_ms: u64,
    /// Maximum delay between retries.
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
        }
    }
}

impl RetryPolicy {
    /// Compute delay for a given attempt (0-indexed) with exponential
    /// backoff and +/-25% jitter.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.base_delay_ms;
        let exponential = base.saturating_mul(1u64 << attempt.min(10));
        let capped = exponential.min(self.max_delay_ms);

        // +/- 25% jitter
        let jitter_range = capped / 4;
        let jitter = if jitter_range > 0 {
            let mut rng = rand::rng();
            rng.random_range(0..=jitter_range * 2) as i64 - jitter_range as i64
        } else {
            0
        };

        let final_ms = (capped as i64 + jitter).max(100) as u64;
        Duration::from_millis(final_ms)
    }

    /// Whether the policy allows any retries.
    pub fn has_retries(&self) -> bool {
        self.max_retries > 0
    }
}

/// Classify a rig StreamingError as retryable or permanent.
pub fn is_retryable_streaming_error(error: &rig::agent::StreamingError) -> bool {
    let classified = crate::error::classify_completion_error(error);
    classified.is_retryable()
}

/// Build a RetriesExhausted error from the last attempt's error.
pub fn retries_exhausted(policy: &RetryPolicy, last_error: &InferenceError) -> InferenceError {
    InferenceError::RetriesExhausted {
        max_retries: policy.max_retries,
        last_error: last_error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay_ms, 1000);
        assert!(policy.has_retries());
    }

    #[test]
    fn no_retry_policy() {
        let policy = RetryPolicy {
            max_retries: 0,
            ..Default::default()
        };
        assert!(!policy.has_retries());
    }

    #[test]
    fn exponential_backoff_increases() {
        let policy = RetryPolicy {
            max_retries: 5,
            base_delay_ms: 1000,
            max_delay_ms: 60_000,
        };

        // Run multiple times to average out jitter
        let mut avg_delays = Vec::new();
        for attempt in 0..4 {
            let mut total = 0u128;
            let runs = 100;
            for _ in 0..runs {
                total += policy.delay_for_attempt(attempt).as_millis();
            }
            avg_delays.push(total / runs);
        }

        // Each attempt should roughly double (within jitter bounds)
        assert!(avg_delays[1] > avg_delays[0], "attempt 1 > attempt 0");
        assert!(avg_delays[2] > avg_delays[1], "attempt 2 > attempt 1");
    }

    #[test]
    fn delay_respects_max() {
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 5000,
        };

        // Even at high attempt count, should not exceed max + jitter
        for _ in 0..50 {
            let delay = policy.delay_for_attempt(20);
            // max_delay + 25% jitter ceiling
            assert!(delay.as_millis() <= 6250);
        }
    }

    #[test]
    fn delay_has_minimum_floor() {
        let policy = RetryPolicy {
            max_retries: 1,
            base_delay_ms: 10,
            max_delay_ms: 10,
        };

        for _ in 0..50 {
            assert!(policy.delay_for_attempt(0).as_millis() >= 100);
        }
    }
}
