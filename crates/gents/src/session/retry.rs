use super::*;
use crate::retry::{
    defradb_conflict_retry_backoff, execute_graphql_with_conflict_retry,
    is_defradb_transaction_conflict_text, DEFRA_DB_CONFLICT_MAX_RETRIES,
};

pub(crate) async fn execute_mutation_with_retry(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &str,
) -> Result<QueryResponse> {
    let resp = execute_graphql_with_conflict_retry(node, mutation, operation).await;
    if resp.has_errors() {
        tracing::warn!(
            operation = %operation,
            errors = ?resp.errors,
            "mutation failed"
        );
        anyhow::bail!("{operation} failed: {:?}", resp.errors);
    }

    Ok(resp)
}

pub(crate) async fn retry_operation<F, Fut, T>(operation: &str, f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_error = None;
    for attempt in 0..=DEFRA_DB_CONFLICT_MAX_RETRIES {
        if attempt > 0 {
            let backoff = defradb_conflict_retry_backoff(attempt - 1);
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
                let retryable = is_defradb_transaction_conflict_text(&error.to_string());
                tracing::warn!(
                    operation = %operation,
                    attempt = attempt,
                    error = %error,
                    "operation failed"
                );
                if retryable && attempt < DEFRA_DB_CONFLICT_MAX_RETRIES {
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
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
