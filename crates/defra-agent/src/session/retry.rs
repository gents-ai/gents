use super::*;

const DEFAULT_BATCH_MUTATION_SIZE: usize = 50;

pub(crate) async fn execute_mutation_with_retry(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &str,
) -> Result<QueryResponse> {
    let mut last_resp = None;
    for attempt in 0..=MAX_MUTATION_RETRIES {
        if attempt > 0 {
            let backoff = Duration::from_millis(INITIAL_RETRY_BACKOFF_MS * (1u64 << (attempt - 1)));
            tracing::warn!(
                operation = %operation,
                attempt = attempt,
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

        tracing::warn!(
            operation = %operation,
            attempt = attempt,
            errors = ?resp.errors,
            elapsed_ms = elapsed.as_millis() as u64,
            "mutation failed"
        );
        last_resp = Some(resp);
    }

    let resp = last_resp.expect("retry loop always sets last_resp before failure");
    anyhow::bail!(
        "{operation} failed after {MAX_MUTATION_RETRIES} retries: {:?}",
        resp.errors
    )
}

pub(crate) async fn execute_batch_mutation_with_retry(
    node: &EmbeddedNode,
    mutation_fields: &[String],
    operation: &str,
) -> Result<()> {
    if mutation_fields.is_empty() {
        return Ok(());
    }

    for fields in mutation_fields.chunks(DEFAULT_BATCH_MUTATION_SIZE) {
        let mutation = build_batch_mutation(fields);
        execute_mutation_with_retry(node, &mutation, operation).await?;
    }

    Ok(())
}

fn build_batch_mutation(fields: &[String]) -> String {
    format!("mutation {{\n{}\n}}", fields.join("\n"))
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
    use super::build_batch_mutation;

    #[test]
    fn build_batch_mutation_wraps_alias_fields() {
        let fields = vec![
            r#"msg_0: create_AgentMessage(input: { message_key: "s:1" }) { _docID }"#.to_string(),
            r#"msg_1: create_AgentMessage(input: { message_key: "s:2" }) { _docID }"#.to_string(),
        ];

        assert_eq!(
            build_batch_mutation(&fields),
            r#"mutation {
msg_0: create_AgentMessage(input: { message_key: "s:1" }) { _docID }
msg_1: create_AgentMessage(input: { message_key: "s:2" }) { _docID }
}"#
        );
    }
}
