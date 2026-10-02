//! Why a request stopped, read from its durable rows.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;

/// The failed inference call that ended a request.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FailedCall {
    pub(crate) failure_reason: Option<String>,
}

/// The request's latest failed call by `ended_at`. A call retried with a
/// higher `attempt` can end before a later call of another kind (compaction)
/// that actually stopped the request.
pub(crate) async fn last_failed_call_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
) -> Result<Option<FailedCall>> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_id: {{ _eq: "{request_id}" }}, call_state: {{ _eq: "failed" }} }},
                order: [{{ ended_at: DESC }}, {{ attempt: DESC }}],
                limit: 1
            ) {{ failure_reason }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .context("InferenceCall query omitted rows")?;
    rows.first()
        .map(|row| serde_json::from_value(row.clone()).context("decode failed InferenceCall"))
        .transpose()
}
