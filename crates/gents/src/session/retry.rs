use super::*;
use crate::retry::{
    defradb_conflict_retry_backoff, is_defradb_transaction_conflict_text,
    DEFRA_DB_CONFLICT_MAX_RETRIES,
};

pub(super) use crate::graphql::graphql_response_with_transaction_retry as execute_query_timed;
pub(crate) use crate::graphql::graphql_with_transaction_retry as execute_mutation_with_retry;

pub(super) async fn retry_operation<F, Fut, T>(operation: &str, f: F) -> Result<T>
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

pub async fn count_active_sessions(node: &EmbeddedNode) -> Result<usize> {
    let query = r#"{
        AgentSession(
            filter: { status: { _eq: "active" } }
        ) {
            _docID
        }
    }"#;

    let response =
        crate::graphql::graphql_with_transaction_retry(node, query, "count active sessions")
            .await?;
    Ok(crate::graphql::rows::<serde_json::Value>(&response, "AgentSession")?.len())
}
