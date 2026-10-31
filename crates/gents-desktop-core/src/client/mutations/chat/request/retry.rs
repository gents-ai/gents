use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::request::lifecycle::{ExecutionOrigin, DEFAULT_REQUEST_MAX_RETRIES};
use gents::store::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use gents::{build_signed_request, RequestIdentity, RequestSigner, RequestSpec, RetryLink};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::super::super::graphql::{
    escape_graphql_string, normalize_optional_string, normalize_required,
};
use super::super::binding::resolve_agent_binding;
use super::prepare::{canonical_request_created_at_after, normalize_authored_content};
use super::SubmittedRequest;

pub async fn retry_request(
    node: &Arc<EmbeddedNode>,
    store: &ClientStore,
    parent: &AgentRequestRow,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
) -> Result<SubmittedRequest> {
    retry_request_with_request_id(
        node,
        store,
        parent,
        requester_did,
        signer,
        admission,
        Uuid::new_v4().to_string(),
    )
    .await
}

pub(super) async fn retry_request_with_request_id(
    node: &Arc<EmbeddedNode>,
    store: &ClientStore,
    parent: &AgentRequestRow,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    request_id: String,
) -> Result<SubmittedRequest> {
    let request_id = normalize_required("new_request_id", &request_id)?.to_string();
    let parent_request_id = normalize_required("request_id", &parent.request_id)?;
    let node_did = normalize_required(
        "node_did",
        parent
            .node_did
            .as_deref()
            .context("retry parent request must have a node_did")?,
    )?;
    let requester_did = normalize_required("requester_did", requester_did)?;

    let request_id_ref = &request_id;
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::FiveAttempts,
        "desktop.request.retry",
        move |txn| {
            let admission = admission.clone();
            Box::pin(async move {
                retry_request_in_txn(
                    txn,
                    store,
                    parent_request_id,
                    node_did,
                    requester_did,
                    signer,
                    admission,
                    request_id_ref,
                )
                .await
            })
        },
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "admission inputs from separate owners (store, identity, admission record)"
)]
async fn retry_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    store: &ClientStore,
    parent_request_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    candidate_request_id: &str,
) -> Result<SubmittedRequest> {
    let (parent_doc_id, parent) =
        load_retry_parent_in_txn(txn, parent_request_id, node_did, requester_did).await?;
    let retry_key = retry_successor_key(&parent_doc_id);
    if let Some(existing) = load_retry_successor_in_txn(txn, &retry_key).await? {
        return Ok(existing);
    }
    let parent_session_id = normalize_required(
        "session_id",
        parent
            .session_id
            .as_deref()
            .context("retry parent request must have a session_id")?,
    )?;
    let input = parent
        .input
        .clone()
        .unwrap_or_default()
        .reissued(&parent_doc_id);
    let content = normalize_authored_content(
        &input,
        parent
            .content
            .as_deref()
            .context("retry parent request must have content")?,
    )?;
    let execution_origin = normalize_required(
        "execution_origin",
        parent
            .execution_origin
            .as_deref()
            .context("retry parent request must have an execution_origin")?,
    )?;
    let retry_count = parent.retry_count.unwrap_or_default() + 1;
    let max_retries = parent
        .max_retries
        .unwrap_or(i64::from(DEFAULT_REQUEST_MAX_RETRIES));
    ensure_retry_parent_eligible(&parent, retry_count - 1, max_retries, execution_origin)?;

    let effective_latest_request_id =
        latest_interactive_request_in_txn(txn, parent_session_id, node_did, requester_did).await?;
    if effective_latest_request_id != parent_request_id {
        bail!(
            "retry parent request must be latest for session {parent_session_id}, got latest_request_id={effective_latest_request_id}"
        );
    }
    ensure_retry_candidate_is_fresh_in_txn(txn, parent_session_id, candidate_request_id).await?;

    let agent_id = normalize_optional_string(parent.agent_id.as_deref());
    let retry_root_request = normalize_optional_string(parent.retry_root_request.as_deref())
        .unwrap_or(parent_request_id);
    let created_at = canonical_request_created_at_after(parent.created_at.as_deref())?;
    let binding = resolve_agent_binding(store, node_did, agent_id, Some(parent_session_id))?;
    let create = build_signed_request(
        RequestSpec {
            retry: Some(RetryLink {
                parent_request_id: Some(parent_request_id.to_string()),
                parent_request_doc_id: Some(parent_doc_id),
                root_request_id: retry_root_request.to_string(),
                retry_count,
                max_retries,
            }),
            retry_key: Some(retry_key),
            input,
            ..RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                RequestIdentity {
                    request_id: candidate_request_id.to_string(),
                    node_did: node_did.to_string(),
                    requester_did: Some(requester_did.to_string()),
                    agent_id: binding.agent_id.clone().unwrap_or_default(),
                    session_id: parent_session_id.to_string(),
                    content: content.to_string(),
                    execution_origin: ExecutionOrigin::from_persisted(Some(execution_origin))?,
                    created_at,
                },
                admission,
            )
        },
        RequestSigner::Identity(signer),
    )
    .await?;
    txn.execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
        .await?;

    Ok(SubmittedRequest {
        request_id: candidate_request_id.to_string(),
        session_id: parent_session_id.to_string(),
        node_did: node_did.to_string(),
        agent_id: binding.agent_id,
    })
}

fn retry_successor_key(parent_request_doc_id: &str) -> String {
    format!("retry:doc:{parent_request_doc_id}")
}

async fn load_retry_successor_in_txn(
    txn: &ConfigApplyTxn<'_>,
    retry_key: &str,
) -> Result<Option<SubmittedRequest>> {
    let retry_key = escape_graphql_string(retry_key);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ retry_key: {{ _eq: "{retry_key}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                node_did
                agent_id
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let Some(row) = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    else {
        return Ok(None);
    };
    Ok(Some(SubmittedRequest {
        request_id: row
            .get("request_id")
            .and_then(Value::as_str)
            .context("retry successor has no request_id")?
            .to_string(),
        session_id: row
            .get("session_id")
            .and_then(Value::as_str)
            .context("retry successor has no session_id")?
            .to_string(),
        node_did: row
            .get("node_did")
            .and_then(Value::as_str)
            .context("retry successor has no node_did")?
            .to_string(),
        agent_id: row
            .get("agent_id")
            .and_then(Value::as_str)
            .and_then(|value| normalize_optional_string(Some(value)))
            .map(str::to_string),
    }))
}

async fn load_retry_parent_in_txn(
    txn: &ConfigApplyTxn<'_>,
    parent_request_id: &str,
    node_did: &str,
    requester_did: &str,
) -> Result<(String, AgentRequestRow)> {
    let parent_request_id = escape_graphql_string(parent_request_id);
    let node_did = escape_graphql_string(node_did);
    let requester_did = escape_graphql_string(requester_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{parent_request_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    requester_did: {{ _eq: "{requester_did}" }}
                }},
                limit: 2
            ) {{
                _docID
                request_id
                node_did
                requester_did
                agent_id
                session_id
                retry_parent_request
                retry_root_request
                superseded_by_request
                content
                input
                max_total_tokens
                lifecycle_state
                backend_id
                execution_origin
                caused_by_trigger_id
                caused_by_trigger_kind
                caused_by_parent_request_id
                failure_reason
                terminalized_at
                terminal_redrive_attempts
                created_at
                claimed_at
                deadline
                retry_count
                max_retries
                interrupt_requested_at
                valid_until
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match rows.len() {
        0 => bail!("retry parent request not found"),
        1 => {}
        count => bail!("retry parent request_id is ambiguous across {count} documents"),
    }
    let row = rows
        .into_iter()
        .next()
        .context("retry parent request absent")?;
    let doc_id = row
        .get("_docID")
        .and_then(Value::as_str)
        .context("retry parent request has no _docID")?
        .to_string();
    let request = serde_json::from_value(row).context("decoding retry parent request")?;
    Ok((doc_id, request))
}

async fn latest_interactive_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
) -> Result<String> {
    let node = escape_graphql_string(node_did);
    let requester = escape_graphql_string(requester_did);
    let session = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session}" }},
                    node_did: {{ _eq: "{node}" }},
                    requester_did: {{ _eq: "{requester}" }}
                }},
                order: [{{ created_at: DESC }}, {{ request_id: DESC }}]
            ) {{ request_id }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .next()
        .and_then(|row| row.get("request_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("retry parent session has no request")
}

async fn ensure_retry_candidate_is_fresh_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    candidate_request_id: &str,
) -> Result<()> {
    let session_id = escape_graphql_string(session_id);
    let candidate_request_id = escape_graphql_string(candidate_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    request_id: {{ _eq: "{candidate_request_id}" }}
                }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let exists = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty());
    if exists {
        bail!("retry new request id already exists: request_id={candidate_request_id}");
    }
    Ok(())
}

fn ensure_retry_parent_eligible(
    parent: &AgentRequestRow,
    parent_retry_count: i64,
    max_retries: i64,
    execution_origin: &str,
) -> Result<()> {
    // The Lean `.released` admission predicate is not persisted on
    // `AgentRequestRow`; it is represented here by requiring the parent to be
    // terminal failed.
    let lifecycle_state = parent
        .lifecycle_state
        .context("retry parent request must have a lifecycle_state")?;

    if lifecycle_state != RequestLifecycleState::Failed {
        bail!("retry parent request must be failed, got lifecycle_state={lifecycle_state}");
    }
    if execution_origin != "interactive" {
        bail!(
            "client retry parent request must be interactive, got execution_origin={execution_origin}"
        );
    }
    if parent_retry_count >= max_retries {
        bail!(
            "retry parent request exhausted retry budget: retry_count={parent_retry_count} max_retries={max_retries}"
        );
    }
    if let Some(deadline) = normalize_optional_string(parent.deadline.as_deref()) {
        let deadline = DateTime::parse_from_rfc3339(deadline)
            .with_context(|| format!("retry parent request has invalid deadline {deadline:?}"))?
            .with_timezone(&Utc);
        if Utc::now() > deadline {
            bail!(
                "retry parent request deadline is closed: deadline={}",
                deadline.to_rfc3339()
            );
        }
    }

    Ok(())
}
