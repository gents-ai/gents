//! Inference retry with exponential backoff and jitter.

use std::time::Duration;

use defra_node::{EmbeddedNode, QueryResponse};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::error::InferenceError;

pub const DEFRA_DB_CONFLICT_MAX_RETRIES: u32 = 3;
pub const DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS: u64 = 100;
pub const TERMINAL_PERSISTENCE_MAX_RETRIES: u32 = 3;
pub const TERMINAL_PERSISTENCE_INITIAL_BACKOFF_MS: u64 = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
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
    /// backoff and +/-25% jitter.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.base_delay_ms;
        let exponential = base.saturating_mul(1u64 << attempt.min(10));
        let capped = exponential.min(self.max_delay_ms);

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

    pub fn has_retries(&self) -> bool {
        self.max_retries > 0
    }
}

pub fn is_retryable_streaming_error(error: &rig::agent::StreamingError) -> bool {
    let classified = crate::error::classify_completion_error(error);
    classified.is_retryable()
}

pub fn is_defradb_transaction_conflict_text(text: &str) -> bool {
    text.to_ascii_lowercase().contains("transaction conflict")
}

pub fn query_response_has_defradb_transaction_conflict(response: &QueryResponse) -> bool {
    response
        .errors
        .iter()
        .any(|error| is_defradb_transaction_conflict_text(&error.message))
}

pub fn defradb_conflict_retry_backoff(retry_index: u32) -> Duration {
    Duration::from_millis(
        DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS.saturating_mul(1u64 << retry_index.min(10)),
    )
}

pub async fn execute_graphql_with_conflict_retry(
    node: &EmbeddedNode,
    graphql: &str,
    operation: &str,
) -> QueryResponse {
    let mut retry_count = 0;
    loop {
        let started = std::time::Instant::now();
        let response = node.execute(graphql).await;
        let elapsed = started.elapsed();

        if !query_response_has_defradb_transaction_conflict(&response)
            || retry_count >= DEFRA_DB_CONFLICT_MAX_RETRIES
        {
            if elapsed > Duration::from_secs(1) {
                tracing::warn!(
                    operation = %operation,
                    retry_count,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "DefraDB GraphQL completed"
                );
            } else {
                tracing::debug!(
                    operation = %operation,
                    retry_count,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "DefraDB GraphQL completed"
                );
            }
            return response;
        }

        let backoff = defradb_conflict_retry_backoff(retry_count);
        tracing::warn!(
            operation = %operation,
            retry_count = retry_count + 1,
            max_retries = DEFRA_DB_CONFLICT_MAX_RETRIES,
            backoff_ms = backoff.as_millis() as u64,
            errors = ?response.errors,
            elapsed_ms = elapsed.as_millis() as u64,
            "retrying DefraDB GraphQL after transaction conflict"
        );
        tokio::time::sleep(backoff).await;
        retry_count += 1;
    }
}

/// are idempotent and guarded by source state, so retrying an ambiguous or
pub(crate) async fn retry_terminal_persistence_operation<T, F, Fut>(
    operation: &str,
    max_retries: u32,
    initial_backoff: Duration,
    mut attempt_operation: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut retry_index = 0;
    loop {
        match attempt_operation().await {
            Ok(value) => return Ok(value),
            Err(error) if retry_index < max_retries => {
                let backoff = initial_backoff.saturating_mul(1u32 << retry_index.min(10));
                tracing::warn!(
                    operation,
                    attempt = retry_index + 1,
                    max_retries,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %error,
                    "retrying terminal persistence after storage failure"
                );
                tokio::time::sleep(backoff).await;
                retry_index += 1;
            }
            Err(error) => {
                tracing::error!(
                    operation,
                    attempts = retry_index + 1,
                    max_retries,
                    error = %error,
                    "terminal persistence retries exhausted; durable repair remains pending"
                );
                return Err(error);
            }
        }
    }
}

pub(crate) async fn execute_graphql_with_terminal_persistence_retry(
    node: &EmbeddedNode,
    graphql: &str,
    operation: &str,
) -> anyhow::Result<QueryResponse> {
    retry_terminal_persistence_operation(
        operation,
        TERMINAL_PERSISTENCE_MAX_RETRIES,
        Duration::from_millis(TERMINAL_PERSISTENCE_INITIAL_BACKOFF_MS),
        || async {
            let response = node.execute(graphql).await;
            if response.has_errors() {
                anyhow::bail!("{operation} failed: {:?}", response.errors);
            }
            Ok(response)
        },
    )
    .await
}

pub fn retries_exhausted(policy: &RetryPolicy, last_error: &InferenceError) -> InferenceError {
    InferenceError::RetriesExhausted {
        max_retries: policy.max_retries,
        last_error: last_error.to_string(),
    }
}

#[cfg(test)]
mod tests;
