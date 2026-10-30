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
                let drained_request_ids =
                    crate::request::lifecycle::queue::drain_all_pending_session_requests_in_txn(
                        txn,
                        &session_id,
                        &node_did,
                        requester_did.as_deref(),
                        "request interrupted because the owning session was stopped",
                    )
                    .await?;
                let interrupted_request_id =
                    crate::request::interrupt::interrupt_active_session_in_txn(
                        txn,
                        &session_id,
                        &node_did,
                        requester_did.as_deref(),
                    )
                    .await?;
                if interrupted_request_id.is_none() {
                    crate::goal::pause_goal_for_interrupt_in_txn(txn, &node_did, &session_id)
                        .await?;
                }
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

/// Resume uses the target node runtime's existing execution registry and job driver as quiescence
/// witnesses; callers must not substitute fresh owners. This relies on the one-active-runtime-per-
/// node operating convention while process and job ownership is checked with the session latch.
pub async fn resume_session(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    background_executions: crate::tools::call::hook::BackgroundExecutionRegistry,
    job_driver: Arc<crate::tools::background::job::driver::JobDriver>,
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
    let identity =
        Arc::new(crate::store::identity::RegisteredIdentity::from_registered_did(&node_did, None)?);
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
            let background_executions = background_executions.clone();
            let job_driver = job_driver.clone();
            Box::pin(async move {
                let owner = crate::session::load_agent_session_row_in_txn(
                    txn,
                    &node_did,
                    &session_id,
                    requester_did.as_deref(),
                )
                .await?
                .context("cannot resume a missing or differently scoped session")?;
                ensure_session_executions_settled_in_txn(
                    txn,
                    &node_did,
                    &session_id,
                    requester_did.as_deref(),
                    &background_executions,
                    &job_driver,
                )
                .await?;
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

async fn ensure_session_executions_settled_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    background_executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
    job_driver: &crate::tools::background::job::driver::JobDriver,
) -> Result<()> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let unsettled = session_tool_call_exists_in_txn(
        txn,
        &format!(
            r#"filter: {{ {scope}, lifecycle_state: {{ _nin: ["completed", "failed", "timedOut", "cancelled"] }} }}"#
        ),
    )
    .await?;
    anyhow::ensure!(!unsettled, "session-owned request or tool execution is still settling; retry agent_resume after agent_session_read reports completion");

    anyhow::ensure!(
        !session_doc_exists_in_txn(
            txn,
            &scope,
            background_executions.active_tool_call_doc_ids()
        )
        .await?,
        "session-owned background execution is still active; retry agent_resume after it settles"
    );
    let executions = background_executions.clone();
    let unexited = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>(
            executions
                .try_process_record_list()?
                .into_iter()
                .filter(|record| {
                    record.identity.observe()
                        != crate::tools::exec::ownership::ProcessObservation::Exited
                })
                .map(|record| record.tool_call_doc_id)
                .collect::<Vec<_>>(),
        )
    })
    .await
    .context("inspecting retained process ownership before resume")?
    .context(
        "cannot verify retained process ownership before resume; retry after runtime recovery",
    )?;
    anyhow::ensure!(
        !session_doc_exists_in_txn(txn, &scope, unexited).await?,
        "session-owned process has not been observed exited; retry agent_resume after settlement"
    );
    anyhow::ensure!(
        !session_doc_exists_in_txn(txn, &scope, job_driver.in_flight_row_ids()).await?,
        "session-owned plugin job is still executing; retry agent_resume after its step settles"
    );
    Ok(())
}

async fn session_doc_exists_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    scope: &str,
    doc_ids: impl IntoIterator<Item = String>,
) -> Result<bool> {
    let doc_ids = doc_ids
        .into_iter()
        .map(|id| format!("\"{}\"", crate::graphql::escape_graphql_string(&id)))
        .collect::<Vec<_>>();
    if doc_ids.is_empty() {
        return Ok(false);
    }
    session_tool_call_exists_in_txn(
        txn,
        &format!(
            "filter: {{ {scope}, _docID: {{ _in: [{}] }} }}, showDeleted: true",
            doc_ids.join(", ")
        ),
    )
    .await
}

async fn session_tool_call_exists_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    arguments: &str,
) -> Result<bool> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentToolCall({arguments}, limit: 1) {{ _docID }} }}"#
        ))
        .await?;
    Ok(!response
        .pointer("/data/AgentToolCall")
        .and_then(serde_json::Value::as_array)
        .context("session execution query omitted AgentToolCall rows; retry resume")?
        .is_empty())
}
