use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::lifecycle::TriggerLineage;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use std::sync::Arc;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::super::graphql::escape_graphql_string;
#[cfg(test)]
use super::super::graphql::normalize_required;
use prepare::prepare_request_submission;

/// The durable submission receipt. Replica observation may still be pending;
/// callers must not replay the mutation when a subsequent refresh fails.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SubmitRequestOptions {
    pub caller_intent: Option<SubmissionIntent>,
    pub valid_until: Option<DateTime<Utc>>,
    pub retry_parent_request: Option<String>,
    pub input: RequestInput,
    pub caused_by_source_doc_id: Option<String>,
    pub trigger_lineage: TriggerLineage,
    pub retry_key: Option<String>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "admission inputs from separate owners (store, access, identity, admission record)"
)]
pub async fn submit_request(
    node: &Arc<EmbeddedNode>,
    store: &ClientStore,
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_id: Option<&str>,
    options: SubmitRequestOptions,
) -> Result<SubmittedRequest> {
    let caller_intent = options.caller_intent.clone();
    if let Some(intent) = caller_intent.as_ref() {
        anyhow::ensure!(
            intent.node_did == node_did,
            "caller submission belongs to another node"
        );
        if let Some(record) = receipt::journal::caller::recover(node, intent).await? {
            return receipt::delivery::reconcile(node, Some(access), record).await;
        }
    }
    let (result, create) = prepare_request_submission(
        node,
        store,
        session_id,
        node_did,
        requester_did,
        signer,
        admission,
        content,
        agent_id,
        options,
        caller_intent
            .as_ref()
            .map(|intent| intent.request_id.as_str()),
    )
    .await?;
    // Owner submissions, including resends routed through this function, are
    // receipt-reconciled. An unknown outcome returns `SubmissionOutcomeUnknown`
    // carrying the original signed payload, so reconciliation replays identical
    // bytes instead of minting a new request identity.
    let pending = receipt::UnresolvedSubmission::new(create, result)?;
    if let Some(intent) = caller_intent {
        let proposal = receipt::correlated::Record::proposal(intent, pending);
        let record = receipt::journal::caller::reserve(node, &proposal).await?;
        return receipt::delivery::reconcile(node, Some(access), record).await;
    }
    receipt::journal::save(node, &pending).await?;
    let result = receipt::commit_signed_submission(access, pending.clone()).await;
    if !result
        .as_ref()
        .err()
        .is_some_and(|error| error.is::<receipt::SubmissionOutcomeUnknown>())
    {
        receipt::journal::forget(node, &pending.request_id).await;
    }
    result
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn submit_goal_backed_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_id: Option<&str>,
    mut options: SubmitRequestOptions,
    objective: &str,
    token_budget: Option<i64>,
) -> Result<SubmittedRequest> {
    if options.retry_key.is_none() {
        options.retry_key = Some(format!(
            "desktop-goal-request:{}",
            normalize_required("session_id", session_id)?
        ));
    }
    let (result, create) = prepare_request_submission(
        node,
        store,
        session_id,
        node_did,
        requester_did,
        signer,
        admission,
        content,
        agent_id,
        options,
        None,
    )
    .await?;
    gents::goal::submit_goal_backed_request(
        access,
        node_did,
        session_id,
        objective,
        token_budget,
        &create,
    )
    .await?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub async fn submit_task_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    access: &ConfigAccess,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_id: Option<&str>,
    mut options: SubmitRequestOptions,
) -> Result<SubmittedRequest> {
    options.retry_key = Some(fire.fire_key.clone());
    let (result, create) = prepare_request_submission(
        node,
        store,
        &fire.session_id,
        &fire.identity.owner_did,
        requester_did,
        signer,
        admission,
        content,
        agent_id,
        options,
        Some(&fire.request_id),
    )
    .await?;
    let admitted = gents::lifecycle::write_task_delivery(access, fire, false, &create).await?;
    Ok(SubmittedRequest {
        request_id: admitted.request.request_id,
        session_id: admitted.request.session_id,
        ..result
    })
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn build_add_agent_request_field(
    alias: &str,
    request_id: &str,
    node_did: &str,
    requester_did: &str,
    agent_id: &str,
    session_id: &str,
    retry_parent_request: &str,
    retry_parent_request_doc_id: Option<&str>,
    retry_root_request: &str,
    content: &str,
    created_at: &str,
    retry_count: i64,
    max_retries: i64,
    backend_id: &str,
    execution_origin: &str,
    extra_fields: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_node_did = escape_graphql_string(node_did);
    let escaped_requester_did = escape_graphql_string(requester_did);
    let escaped_agent_id = escape_graphql_string(agent_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_retry_parent = escape_graphql_string(retry_parent_request);
    let retry_parent_doc_field = retry_parent_request_doc_id
        .map(escape_graphql_string)
        .map(|doc_id| format!(r#"retry_parent_request_doc_id: "{doc_id}","#))
        .unwrap_or_default();
    let escaped_retry_root = escape_graphql_string(retry_root_request);
    let escaped_content = escape_graphql_string(content);
    let escaped_created_at = escape_graphql_string(created_at);
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_execution_origin = escape_graphql_string(execution_origin);

    format!(
        r#"{alias}: add_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                node_did: "{escaped_node_did}",
                requester_did: "{escaped_requester_did}",
                agent_id: "{escaped_agent_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "{escaped_retry_parent}",
                {retry_parent_doc_field}
                retry_root_request: "{escaped_retry_root}",
                superseded_by_request: "",
                content: "{escaped_content}",
                lifecycle_state: "pending",
                backend_id: "{escaped_backend_id}",
                execution_origin: "{escaped_execution_origin}",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: {retry_count},
                max_retries: {max_retries}{extra_fields}
            }}) {{ _docID }}
        "#
    )
}

/// Only `lifecycle_state=dead` with `failure_reason=Stale` is eligible — any
/// other state would risk bypassing legitimate terminal classifications.
#[allow(clippy::too_many_arguments)]
pub async fn resend_request(
    node: &Arc<EmbeddedNode>,
    store: &ClientStore,
    access: &ConfigAccess,
    stale_request_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
) -> Result<SubmittedRequest> {
    let stale = fetch_request_view(node, stale_request_id, node_did, requester_did).await?;
    if stale.lifecycle_state != Some(RequestLifecycleState::Dead)
        || stale.failure_reason.as_deref() != Some("Stale")
    {
        anyhow::bail!(
            "request {stale_request_id} is not a stale terminal (lifecycle_state={}, failure_reason={})",
            stale
                .lifecycle_state
                .map_or("<missing>", RequestLifecycleState::as_str),
            stale.failure_reason.as_deref().unwrap_or("<missing>")
        );
    }
    let stale_node_did = stale
        .node_did
        .as_deref()
        .context("stale request has no node_did")?;
    let stale_content = stale
        .content
        .as_deref()
        .context("stale request has no content")?;
    let retry_session_id = Uuid::new_v4().to_string();
    submit_request(
        node,
        store,
        access,
        &retry_session_id,
        stale_node_did,
        requester_did,
        signer,
        admission,
        stale_content,
        stale.agent_id.as_deref(),
        SubmitRequestOptions {
            caller_intent: None,
            valid_until: Some(Utc::now() + chrono::Duration::minutes(5)),
            retry_parent_request: Some(stale_request_id.to_string()),
            input: stale.input.unwrap_or_default(),
            caused_by_source_doc_id: None,
            trigger_lineage: TriggerLineage::default(),
            retry_key: None,
        },
    )
    .await
}

async fn fetch_request_view(
    node: &EmbeddedNode,
    request_id: &str,
    node_did: &str,
    requester_did: &str,
) -> Result<AgentRequestRow> {
    let escaped = escape_graphql_string(request_id);
    let node_did = escape_graphql_string(node_did);
    let requester_did = escape_graphql_string(requester_did);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{escaped}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    requester_did: {{ _eq: "{requester_did}" }}
                }},
                limit: 1
            ) {{
                request_id
                node_did
                agent_id
                content
                input
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    let resp = gents::graphql::graphql_with_transaction_retry(node, &query, "fetch_request")
        .await
        .with_context(|| format!("fetch_request({request_id})"))?;
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    serde_json::from_value(row.clone())
        .with_context(|| format!("decoding AgentRequest {request_id}"))
}

#[cfg(test)]
#[path = "../../../../../gents/src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

#[cfg(test)]
mod tests;

pub(super) mod prepare;
pub(crate) mod receipt;
pub(super) mod retry;

pub use receipt::correlated::{Intent as SubmissionIntent, Reference as SubmissionReference};
pub use receipt::{
    reconcile_unretained_submission, resolve_unresolved_submission, DeliveryUnconfirmed,
    SubmissionNotDelivered, SubmissionOutcomeUnknown, UnresolvedSubmission,
};
