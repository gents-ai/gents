use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionStopReceipt {
    pub session_id: String,
    pub status: &'static str,
    pub interrupted_request_id: Option<String>,
    pub drained_request_ids: Vec<String>,
}

pub async fn stop_session(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<SessionStopReceipt> {
    anyhow::ensure!(
        !session_id.trim().is_empty() && !node_did.trim().is_empty(),
        "session stop requires exact session and node identity"
    );
    anyhow::ensure!(
        node.node_identity_did() == Some(node_did),
        "session stop is supported only on this node; remote control is unavailable"
    );
    let node_did = node_did.to_owned();
    let session_id = session_id.to_owned();
    let requester_did = requester_did.map(str::to_owned);
    let actor = ::identity::Did::new(node_did.clone())?;
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        Some(actor),
        "session.stop",
        move |txn| {
            let node_did = node_did.clone();
            let session_id = session_id.clone();
            let requester_did = requester_did.clone();
            Box::pin(async move {
                let owner = crate::session::load_agent_session_row_in_txn(
                    txn,
                    &node_did,
                    &session_id,
                    requester_did.as_deref(),
                )
                .await?
                .context("cannot stop a missing or differently scoped session")?;
                let stopped_at = chrono::Utc::now().to_rfc3339();
                let doc_id = crate::graphql::escape_graphql_string(&owner.doc_id);
                let scope = crate::session::session_scope_filter(
                    &node_did,
                    &session_id,
                    requester_did.as_deref(),
                );
                let stopped_at = crate::graphql::escape_graphql_string(&stopped_at);
                let response = txn
                    .execute(&format!(
                        r#"mutation {{ update_AgentSession(filter: {{
                            _docID: {{ _eq: "{doc_id}" }}, {scope}
                        }}, input: {{ stopped_at: "{stopped_at}" }}) {{ _docID }} }}"#
                    ))
                    .await?;
                anyhow::ensure!(
                    response.pointer("/data/update_AgentSession/0/_docID")
                        == Some(&serde_json::Value::String(owner.doc_id.clone())),
                    "session stop latch was not committed to the selected session"
                );
                crate::goal::pause_goal_for_interrupt_in_txn(txn, &node_did, &session_id).await?;
                let drained_request_ids =
                    crate::lifecycle::queue::drain_all_pending_session_requests_in_txn(
                        txn,
                        &session_id,
                        &node_did,
                        requester_did.as_deref(),
                        "request interrupted because the owning session was stopped",
                    )
                    .await?;
                let interrupted_request_id = crate::interrupt::interrupt_active_session_in_txn(
                    txn,
                    &session_id,
                    &node_did,
                    requester_did.as_deref(),
                )
                .await?;
                Ok(SessionStopReceipt {
                    session_id,
                    status: if interrupted_request_id.is_some() {
                        "stopping"
                    } else {
                        "stop_requested"
                    },
                    interrupted_request_id,
                    drained_request_ids,
                })
            })
        },
    )
    .await
}

pub async fn resume_session(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !session_id.trim().is_empty() && !node_did.trim().is_empty(),
        "session resume requires exact session and node identity"
    );
    anyhow::ensure!(
        node.node_identity_did() == Some(node_did),
        "session resume is supported only on this node; remote control is unavailable"
    );
    let node_did = node_did.to_owned();
    let session_id = session_id.to_owned();
    let requester_did = requester_did.map(str::to_owned);
    let identity = Arc::new(crate::identity::RegisteredIdentity::from_registered_did(
        &node_did, None,
    )?);
    let actor = ::identity::Did::new(node_did.clone())?;
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        Some(actor),
        "session.resume",
        move |txn| {
            let identity = identity.clone();
            let node_did = node_did.clone();
            let session_id = session_id.clone();
            let requester_did = requester_did.clone();
            Box::pin(async move {
                let owner = crate::session::load_agent_session_row_in_txn(
                    txn,
                    &node_did,
                    &session_id,
                    requester_did.as_deref(),
                )
                .await?
                .context("cannot resume a missing or differently scoped session")?;
                let doc_id = crate::graphql::escape_graphql_string(&owner.doc_id);
                let scope = crate::session::session_scope_filter(
                    &node_did,
                    &session_id,
                    requester_did.as_deref(),
                );
                let response = txn
                    .execute(&format!(
                        r#"mutation {{ update_AgentSession(filter: {{
                            _docID: {{ _eq: "{doc_id}" }}, {scope}
                        }}, input: {{ stopped_at: null }}) {{ _docID }} }}"#
                    ))
                    .await?;
                anyhow::ensure!(
                    response.pointer("/data/update_AgentSession/0/_docID")
                        == Some(&serde_json::Value::String(owner.doc_id.clone())),
                    "session resume latch was not cleared on the selected session"
                );
                crate::goal::stage_session_resume_in_txn(
                    txn,
                    identity.as_ref(),
                    &node_did,
                    &session_id,
                )
                .await
            })
        },
    )
    .await
}
