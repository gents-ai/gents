use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InferenceCallRecoveryReport {
    pub calls_recovered: usize,
}

pub struct InferenceCall;

#[derive(Debug, Deserialize)]
struct StaleInferenceCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    call_id: String,
    request_id: String,
    request_doc_id: String,
    agent_did: String,
    call_state: String,
}

enum InferenceRecoveryOutcome {
    Cancelled,
    Failed,
}

impl InferenceCall {
    pub async fn recover_all(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<InferenceCallRecoveryReport> {
        let rows = load_stale_inference_calls(node, node_did).await?;
        let mut calls_recovered = 0;

        for row in rows {
            let Some(parent) = lookup_parent_request(node, node_did, &row).await? else {
                continue;
            };
            let Some(outcome) = recovery_outcome(&row, &parent) else {
                continue;
            };

            match recover_inference_call_row(node, &row, outcome).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(
                        call_id = %row.call_id,
                        request_id = %row.request_id,
                        call_state = %row.call_state,
                        error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
                        "failed to recover stale inference call"
                    );
                    continue;
                }
            }

            calls_recovered += 1;
            tracing::info!(
                call_id = %row.call_id,
                request_id = %row.request_id,
                "recovered stale inference call"
            );
        }

        Ok(InferenceCallRecoveryReport { calls_recovered })
    }
}

async fn load_stale_inference_calls(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Vec<StaleInferenceCallRow>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    call_state: {{ _in: ["queued", "running"] }}
                }}
            ) {{
                _docID
                call_id
                request_id
                request_doc_id
                agent_did
                call_state
            }}
        }}"#
    );

    let resp =
        graphql_with_transaction_retry(node, &query, "querying stale InferenceCall rows").await?;

    Ok(crate::store::graphql::required_rows(
        &resp,
        "InferenceCall",
    )?)
}

async fn lookup_parent_request(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    row: &StaleInferenceCallRow,
) -> Result<Option<AgentRequestRow>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let request_id = &row.request_id;
    let escaped_request_id = escape_graphql_string(request_id);
    let request_doc_id = escape_graphql_string(&row.request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }}
                    _docID: {{ _eq: "{request_doc_id}" }}
                }},
                limit: 1
            ) {{
                request_id
                lifecycle_state
            }}
        }}"#
    );

    let resp = crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(identity::Did::new(row.agent_did.clone())?),
        "inference.recovery_parent",
        |txn| {
            let query = &query;
            Box::pin(async move { txn.execute_local_response(query).await })
        },
    )
    .await
    .with_context(|| {
        format!("querying parent request for inference recovery request_id={request_id}")
    })?;

    let rows: Vec<AgentRequestRow> = crate::store::graphql::required_rows(&resp, "AgentRequest")?;
    Ok(rows.into_iter().next())
}

fn recovery_outcome(
    row: &StaleInferenceCallRow,
    parent: &AgentRequestRow,
) -> Option<InferenceRecoveryOutcome> {
    if request_is_interrupted(parent) {
        return Some(InferenceRecoveryOutcome::Cancelled);
    }
    if !request_is_terminal(parent) {
        return None;
    }

    match row.call_state.as_str() {
        "queued" => Some(InferenceRecoveryOutcome::Cancelled),
        "running" => Some(InferenceRecoveryOutcome::Failed),
        _ => None,
    }
}

async fn recover_inference_call_row(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &StaleInferenceCallRow,
    outcome: InferenceRecoveryOutcome,
) -> Result<bool> {
    let escaped_doc_id = escape_graphql_string(&row.doc_id);
    let observed_call_state = escape_graphql_string(&row.call_state);
    let ended_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let (call_state, failure_reason) = match outcome {
        InferenceRecoveryOutcome::Cancelled => ("cancelled", "Cancelled"),
        InferenceRecoveryOutcome::Failed => ("failed", "StreamDroppedBeforeTerminalResponse"),
    };
    let mutation = format!(
        r#"mutation {{
            update_InferenceCall(
                docID: "{escaped_doc_id}",
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    call_state: {{ _eq: "{observed_call_state}" }}
                }},
                input: {{
                    call_state: "{call_state}",
                    failure_reason: "{failure_reason}",
                    ended_at: "{ended_at}"
                }}
            ) {{ _docID }}
        }}"#,
    );

    let response = crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        Some(identity::Did::new(row.agent_did.clone())?),
        "recover_inference_call",
        |txn| {
            let mutation = &mutation;
            Box::pin(async move { txn.execute_local_response(mutation).await })
        },
    )
    .await
    .context("recover inference call mutation")?;
    Ok(!crate::store::graphql::required_rows::<serde_json::Value>(
        &response,
        "update_InferenceCall",
    )?
    .is_empty())
}

fn request_is_interrupted(parent: &AgentRequestRow) -> bool {
    parent.lifecycle_state == Some(RequestLifecycleState::Interrupted)
}

fn request_is_terminal(parent: &AgentRequestRow) -> bool {
    parent
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
