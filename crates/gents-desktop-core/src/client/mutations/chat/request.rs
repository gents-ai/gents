use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents::request::lifecycle::TriggerLineage;
use gents::store::config_client::ConfigAccess;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::super::graphql::escape_graphql_string;
#[cfg(test)]
use super::super::graphql::normalize_required;
use prepare::prepare_request_submission;

/// The request the runtime holds for a send. Replica observation may still be
/// pending; callers must not resend under a new identity when a subsequent
/// refresh fails.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_did: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SubmitRequestOptions {
    /// The request identity the client picked when the person sent (Lean
    /// `ClientOptimisticSend`). A retry passes the same identity, which the
    /// runtime's create treats as the same request; `None` mints one.
    pub request_id: Option<String>,
    pub valid_until: Option<DateTime<Utc>>,
    pub retry_parent_request: Option<String>,
    pub input: RequestInput,
    pub caused_by_source_doc_id: Option<String>,
    pub trigger_lineage: TriggerLineage,
    pub retry_key: Option<String>,
}

/// One signed, idempotent create (`send_client_request`). A failure leaves no
/// request under the identity unless an earlier send of it committed, so the
/// caller shows the message failed and may resend the same identity.
#[expect(
    clippy::too_many_arguments,
    reason = "admission inputs from separate owners (store, access, identity, admission record)"
)]
pub async fn submit_request(
    store: &ClientStore,
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_did: Option<&str>,
    options: SubmitRequestOptions,
) -> Result<SubmittedRequest> {
    let request_id = options.request_id.clone();
    let (result, create) = prepare_request_submission(
        access,
        store,
        session_id,
        node_did,
        requester_did,
        signer,
        admission,
        content,
        agent_did,
        options,
        request_id.as_deref(),
    )
    .await?;
    let held = gents::request::lifecycle::send_client_request(access, &create).await?;
    Ok(SubmittedRequest {
        request_id: held,
        ..result
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn submit_goal_backed_request(
    store: &ClientStore,
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_did: Option<&str>,
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
        access,
        store,
        session_id,
        node_did,
        requester_did,
        signer,
        admission,
        content,
        agent_did,
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
    store: &ClientStore,
    access: &ConfigAccess,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_did: Option<&str>,
    mut options: SubmitRequestOptions,
) -> Result<SubmittedRequest> {
    options.retry_key = Some(fire.fire_key.clone());
    let (result, create) = prepare_request_submission(
        access,
        store,
        &fire.session_id,
        &fire.identity.owner_did,
        requester_did,
        signer,
        admission,
        content,
        agent_did,
        options,
        Some(&fire.request_id),
    )
    .await?;
    let admitted =
        gents::request::lifecycle::write_task_delivery(access, fire, false, &create).await?;
    Ok(SubmittedRequest {
        request_id: admitted.request.request_id,
        session_id: admitted.request.session_id,
        ..result
    })
}

/// Only `lifecycle_state=dead` with `failure_reason=Stale` is eligible — any
/// other state would risk bypassing legitimate terminal classifications.
#[allow(clippy::too_many_arguments)]
pub async fn resend_request(
    store: &ClientStore,
    access: &ConfigAccess,
    stale_request_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
) -> Result<SubmittedRequest> {
    let stale = fetch_request_view(access, stale_request_id, node_did, requester_did).await?;
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
        store,
        access,
        &retry_session_id,
        stale_node_did,
        signer.did(),
        signer,
        admission,
        stale_content,
        stale.agent_did.as_deref(),
        SubmitRequestOptions {
            request_id: None,
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
    access: &ConfigAccess,
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
                agent_did
                content
                input
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    let resp = access
        .execute(&query)
        .await
        .with_context(|| format!("fetch_request({request_id})"))?;
    let row = resp
        .get("data")
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

