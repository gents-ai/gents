use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::graphql::escape_graphql_string;
use crate::session::execute_mutation_with_retry;

#[derive(Debug, Default)]
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
    call_state: String,
}

enum InferenceRecoveryOutcome {
    Cancelled,
    Failed,
}

impl InferenceCall {
    pub async fn recover_all(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<InferenceCallRecoveryReport> {
        let rows = load_stale_inference_calls(node, agent_did).await?;
        let mut calls_recovered = 0;

        for row in rows {
            let Some(parent) = lookup_parent_request(node, agent_did, &row.request_id).await?
            else {
                continue;
            };
            let Some(outcome) = recovery_outcome(&row, &parent) else {
                continue;
            };

            if let Err(error) = recover_inference_call_row(node, &row, outcome).await {
                tracing::warn!(
                    call_id = %row.call_id,
                    request_id = %row.request_id,
                    call_state = %row.call_state,
                    error = %error,
                    "failed to recover stale inference call"
                );
                continue;
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
    agent_did: &str,
) -> Result<Vec<StaleInferenceCallRow>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    call_state: {{ _in: ["queued", "running"] }}
                }}
            ) {{
                _docID
                call_id
                request_id
                call_state
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying stale InferenceCall rows: {:?}", resp.errors);
    }

    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows)
}

async fn lookup_parent_request(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }}
                }},
                limit: 1
            ) {{
                request_id
                lifecycle_state
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "querying parent request for inference recovery request_id={request_id}: {:?}",
            resp.errors
        );
    }

    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&resp, "AgentRequest")?;
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
    node: &EmbeddedNode,
    row: &StaleInferenceCallRow,
    outcome: InferenceRecoveryOutcome,
) -> Result<()> {
    let escaped_doc_id = escape_graphql_string(&row.doc_id);
    let ended_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let (call_state, failure_reason) = match outcome {
        InferenceRecoveryOutcome::Cancelled => ("cancelled", "Cancelled"),
        InferenceRecoveryOutcome::Failed => ("failed", "StreamDroppedBeforeTerminalResponse"),
    };
    let mutation = format!(
        r#"mutation {{
            update_InferenceCall(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                input: {{
                    call_state: "{call_state}",
                    failure_reason: "{failure_reason}",
                    ended_at: "{ended_at}"
                }}
            ) {{ _docID }}
        }}"#,
    );

    execute_mutation_with_retry(node, &mutation, "recover_inference_call")
        .await
        .context("recover inference call mutation")?;
    Ok(())
}

fn request_is_interrupted(parent: &AgentRequestRow) -> bool {
    parent.lifecycle_state == Some(RequestLifecycleState::Interrupted)
}

fn request_is_terminal(parent: &AgentRequestRow) -> bool {
    parent
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
}
