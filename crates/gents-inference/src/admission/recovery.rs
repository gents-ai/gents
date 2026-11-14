use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use gents_store::graphql::escape_graphql_string;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InferenceCallRecoveryReport {
    pub calls_recovered: usize,
}

pub struct InferenceCall;

#[derive(Debug, Deserialize)]
pub struct StaleInferenceCallRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub call_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub agent_did: String,
    pub call_state: String,
}

pub enum InferenceRecoveryOutcome {
    Cancelled,
    Failed,
}

impl InferenceCall {
    pub async fn recover_all_with_identities(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        identities: &[std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    ) -> Result<InferenceCallRecoveryReport> {
        anyhow::ensure!(
            node.node_identity_did() == Some(node_did),
            "inference recovery node identity mismatch"
        );
        let mut calls_recovered = 0;
        for identity in identities {
            anyhow::ensure!(
                identity.did() != node_did,
                "runtime identity cannot recover agent inference"
            );
            let actor = identity::Did::new(identity.did())?;
            let access = match gents_store::config_client::ConfigAccess::local_managed_agent(
                node.clone(),
                actor,
                identity.as_ref(),
            ) {
                Ok(access) => access,
                Err(error) => {
                    tracing::warn!(agent_did = identity.did(), %error, "inference recovery signer unavailable");
                    continue;
                }
            };
            let rows = load_stale_inference_calls(&access, node_did, identity.did()).await?;
            for row in rows {
                let parent = match lookup_parent_request(&access, node_did, &row).await {
                    Ok(parent) => parent,
                    Err(error) => {
                        tracing::warn!(
                            call_id = %row.call_id,
                            request_id = %row.request_id,
                            error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
                            "skipped stale inference call without a visible parent request"
                        );
                        continue;
                    }
                };
                let Some(outcome) = recovery_outcome(&row, &parent) else {
                    continue;
                };

                match recover_inference_call_row(&access, &row, outcome).await {
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
        }
        Ok(InferenceCallRecoveryReport { calls_recovered })
    }
}

pub async fn load_stale_inference_calls(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<Vec<StaleInferenceCallRow>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
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

    let resp = access.execute(&query).await?;
    Ok(serde_json::from_value(
        resp["data"]["InferenceCall"].clone(),
    )?)
}

pub async fn lookup_parent_request(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    row: &StaleInferenceCallRow,
) -> Result<AgentRequestRow> {
    let request_id = &row.request_id;
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(row.agent_did.as_str()),
        "inference recovery actor mismatch"
    );
    let query = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        Query::select(
            Selection::new()
                .field(fields::REQUEST_ID)
                .field(fields::LIFECYCLE_STATE),
        )
        .filter(Filter::eq(fields::NODE_DID, node_did))
        .filter(Filter::eq(fields::AGENT_DID, &row.agent_did))
        .filter(Filter::eq(fields::REQUEST_ID, request_id))
        .filter(Filter::eq(fields::DOC_ID, &row.request_doc_id))
        .limit(1)
        .render()
    };

    let resp = access
        .transact_readonly("inference.recovery_parent", |txn| {
            let query = &query;
            Box::pin(async move { txn.run_local_response(query).await })
        })
        .await
        .with_context(|| {
            format!("querying parent request for inference recovery request_id={request_id}")
        })?;

    let rows: Vec<AgentRequestRow> = gents_store::graphql::required_rows(&resp, "AgentRequest")?;
    // A call row the agent can read names its parent. Authorization filters
    // rows silently, so an empty result means the parent is not visible to
    // this agent (or is gone), never that recovery has nothing to do.
    rows.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!(
            "parent AgentRequest {} for call {} is not visible to inference recovery",
            row.request_doc_id,
            row.call_id
        )
    })
}

pub fn recovery_outcome(
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

pub async fn recover_inference_call_row(
    access: &gents_store::config_client::ConfigAccess,
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

    let response = access
        .transact_write("recover_inference_call", |txn| {
            let mutation = &mutation;
            Box::pin(async move { txn.execute_local_response(mutation).await })
        })
        .await
        .context("recover inference call mutation")?;
    Ok(!gents_store::graphql::required_rows::<serde_json::Value>(
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
