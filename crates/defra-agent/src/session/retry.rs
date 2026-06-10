use super::*;

pub(crate) async fn execute_mutation_with_retry(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &str,
) -> Result<QueryResponse> {
    let mut max_retries = MAX_MUTATION_RETRIES;
    let mut attempt = 0;
    loop {
        if attempt > 0 {
            let backoff = mutation_retry_backoff(operation, mutation, attempt);
            tracing::warn!(
                operation = %operation,
                attempt = attempt,
                max_retries = max_retries,
                backoff_ms = backoff.as_millis() as u64,
                "retrying mutation"
            );
            tokio::time::sleep(backoff).await;
        }

        let started = std::time::Instant::now();
        let resp = node.execute(mutation).await;
        let elapsed = started.elapsed();
        log_mutation_timing(operation, elapsed);

        if !resp.has_errors() {
            return Ok(resp);
        }

        if response_has_transient_mutation_error(&resp) {
            max_retries = max_retries.max(MAX_TRANSIENT_MUTATION_RETRIES);
        }

        tracing::warn!(
            operation = %operation,
            attempt = attempt,
            max_retries = max_retries,
            errors = ?resp.errors,
            elapsed_ms = elapsed.as_millis() as u64,
            "mutation failed"
        );

        if attempt >= max_retries {
            anyhow::bail!(
                "{operation} failed after {max_retries} retries: {:?}",
                resp.errors
            )
        }
        attempt += 1;
    }
}

pub(super) async fn retry_operation<F, Fut, T>(operation: &str, f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_error = None;
    for attempt in 0..=MAX_MUTATION_RETRIES {
        if attempt > 0 {
            let backoff = Duration::from_millis(INITIAL_RETRY_BACKOFF_MS * (1u64 << (attempt - 1)));
            tracing::warn!(
                operation = %operation,
                attempt = attempt,
                backoff_ms = backoff.as_millis() as u64,
                "retrying operation"
            );
            tokio::time::sleep(backoff).await;
        }

        match f().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                tracing::warn!(
                    operation = %operation,
                    attempt = attempt,
                    error = %error,
                    "operation failed"
                );
                last_error = Some(error);
            }
        }
    }

    Err(last_error.expect("retry loop always sets last_error before failure"))
}

pub(super) fn log_mutation_timing(operation: &str, elapsed: Duration) {
    if elapsed > Duration::from_secs(1) {
        tracing::warn!(
            operation = %operation,
            elapsed_ms = elapsed.as_millis() as u64,
            "slow mutation"
        );
    } else {
        tracing::debug!(
            operation = %operation,
            elapsed_ms = elapsed.as_millis() as u64,
            "mutation completed"
        );
    }
}

fn mutation_retry_backoff(operation: &str, mutation: &str, attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(10);
    let base_ms = INITIAL_RETRY_BACKOFF_MS
        .saturating_mul(1u64 << shift)
        .min(MAX_RETRY_BACKOFF_MS);
    Duration::from_millis(base_ms + mutation_retry_jitter_ms(operation, mutation, attempt))
}

fn mutation_retry_jitter_ms(operation: &str, mutation: &str, attempt: u32) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    operation.hash(&mut hasher);
    mutation.hash(&mut hasher);
    attempt.hash(&mut hasher);
    hasher.finish() % (MAX_RETRY_JITTER_MS + 1)
}

fn response_has_transient_mutation_error(resp: &QueryResponse) -> bool {
    if resp.errors.is_empty() {
        return false;
    }
    let rendered = format!("{:?}", resp.errors).to_ascii_lowercase();
    rendered.contains("transaction conflict") || rendered.contains("please retry")
}

pub(super) async fn execute_query_timed(
    node: &EmbeddedNode,
    query: &str,
    operation: &str,
) -> QueryResponse {
    let started = std::time::Instant::now();
    let resp = node.execute(query).await;
    log_mutation_timing(operation, started.elapsed());
    resp
}

pub async fn count_active_sessions(node: &EmbeddedNode) -> Result<usize> {
    let query = r#"{
        AgentSession(
            filter: { status: { _eq: "active" } }
        ) {
            _docID
        }
    }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("counting active sessions: {:?}", resp.errors);
    }

    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(|value| value.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_mutation_error_detection_matches_datastore_conflicts() {
        let resp = QueryResponse::error(
            "datastore error: storage error: transaction conflict. Please retry",
        );

        assert!(response_has_transient_mutation_error(&resp));
    }

    #[test]
    fn transient_mutation_error_detection_leaves_validation_errors_on_default_budget() {
        let resp = QueryResponse::error("field `status` is not defined by type AgentResponse");

        assert!(!response_has_transient_mutation_error(&resp));
    }

    #[test]
    fn mutation_retry_backoff_is_capped_and_staggered_by_mutation() {
        let first = mutation_retry_backoff("flush_streaming_response_snapshot", "mutation-a", 1);
        let second = mutation_retry_backoff("flush_streaming_response_snapshot", "mutation-b", 1);
        let capped = mutation_retry_backoff("flush_streaming_response_snapshot", "mutation-a", 20);

        assert!(first >= Duration::from_millis(INITIAL_RETRY_BACKOFF_MS));
        assert!(first <= Duration::from_millis(INITIAL_RETRY_BACKOFF_MS + MAX_RETRY_JITTER_MS));
        assert_ne!(first, second);
        assert!(capped >= Duration::from_millis(MAX_RETRY_BACKOFF_MS));
        assert!(capped <= Duration::from_millis(MAX_RETRY_BACKOFF_MS + MAX_RETRY_JITTER_MS));
    }
}
