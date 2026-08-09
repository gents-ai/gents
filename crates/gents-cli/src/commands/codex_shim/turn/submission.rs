use std::time::Duration;

use anyhow::Result;

use super::super::ShimState;
use crate::{create_agent_request, RequestSubmitOptions, SubmittedRequest};

pub(super) async fn create_agent_request_with_retry(
    state: &ShimState,
    content: &str,
    session_id: Option<&str>,
    options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    let mut last_error = None;
    for attempt in 0..5 {
        match create_agent_request(
            &state.graphql,
            state.agent_did.as_ref(),
            content,
            session_id,
            Some(state.behavior_id.as_ref()),
            options.clone(),
        )
        .await
        {
            Ok(submitted) => return Ok(submitted),
            Err(error) if graphql_submission_error_is_retryable(&error) && attempt < 4 => {
                tracing::warn!(
                    attempt,
                    error = %error,
                    "retrying Codex shim AgentRequest submission"
                );
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(50 * (attempt + 1) as u64)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.expect("retry loop stores the retryable submission error"))
}

fn graphql_submission_error_is_retryable(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("transaction conflict")
        || message.contains("Transaction conflict")
        || message.contains("database is locked")
}
