use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionStopReceipt {
    pub session_id: String,
    pub status: &'static str,
    pub interrupted_request_id: Option<String>,
    pub drained_request_ids: Vec<String>,
}

pub async fn stop_session_with_access(
    node: &Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
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
    access
        .transact_write("session.stop", move |txn| {
            let node_did = node_did.clone();
            let session_id = session_id.clone();
            let requester_did = requester_did.clone();
            Box::pin(async move {
                let owner =
                    gents_store::config_client::agent_session::load_agent_session_row_in_txn(
                        txn,
                        &node_did,
                        &session_id,
                        requester_did.as_deref(),
                    )
                    .await?
                    .context("cannot stop a missing or differently scoped session")?;
                let stopped_at = chrono::Utc::now().to_rfc3339();
                let response = {
                    use gents_protocol::document_query::{
                        fields::agent_session as fields, session_scope, Filter, Input, Update,
                    };
                    txn.run(
                        &Update::matching(
                            Filter::eq(fields::DOC_ID, owner.doc_id.as_str()),
                            Input::new().set(fields::STOPPED_AT, &stopped_at),
                        )
                        .filter(session_scope(
                            &node_did,
                            &session_id,
                            requester_did.as_deref(),
                        ))
                        .render(),
                    )
                    .await?
                };
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
                    crate::request::hooks::points::session_halt(txn, &node_did, &session_id)
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
        })
        .await
}

/// The process-local witnesses that a session's executions have settled.
/// They observe the hosting runtime's own execution owners; a fresh owner
/// witnesses nothing, so callers pass the runtime's.
pub trait SessionExecutionWitness: Send + Sync {
    /// Tool-call document IDs of the background executions this process runs.
    fn active_tool_call_doc_ids(&self) -> Vec<String>;

    /// The retained process ownership records. Blocking: reads host records.
    fn process_records(&self) -> Result<Vec<crate::exec::ownership::ProcessRecord>>;

    /// Row document IDs of the plugin job steps executing in this process.
    fn in_flight_job_row_ids(&self) -> Vec<String>;
}

/// Resume uses the target node runtime's existing execution registry and job driver as quiescence
/// witnesses; callers must not substitute fresh owners. This relies on the one-active-runtime-per-
/// node operating convention while process and job ownership is checked with the session latch.
pub async fn resume_session_with_access(
    node: &Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    executions: Arc<dyn SessionExecutionWitness>,
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
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = match access.managed_agent_signer()
    {
        Some(signer) => Arc::new(signer),
        None => {
            let actor = access
                .actor_did()
                .context("session resume actor unavailable")?;
            anyhow::ensure!(
                node.node_identity_did() == Some(actor.as_str()),
                "session resume requires a managed Agent signer"
            );
            Arc::new(gents_store::identity::RegisteredIdentity::from_registered_did(actor, None)?)
        }
    };
    access
        .transact_write("session.resume", move |txn| {
            let identity = identity.clone();
            let node_did = node_did.clone();
            let session_id = session_id.clone();
            let requester_did = requester_did.clone();
            let executions = executions.clone();
            Box::pin(async move {
                let owner =
                    gents_store::config_client::agent_session::load_agent_session_row_in_txn(
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
                    &executions,
                )
                .await?;
                let response = {
                    use gents_protocol::document_query::{
                        fields::agent_session as fields, session_scope, Filter, Input, Update,
                    };
                    txn.run(
                        &Update::matching(
                            Filter::eq(fields::DOC_ID, owner.doc_id.as_str()),
                            Input::new().set_null(fields::STOPPED_AT),
                        )
                        .filter(session_scope(
                            &node_did,
                            &session_id,
                            requester_did.as_deref(),
                        ))
                        .render(),
                    )
                    .await?
                };
                anyhow::ensure!(
                    response.pointer("/data/update_AgentSession/0/_docID")
                        == Some(&serde_json::Value::String(owner.doc_id.clone())),
                    "session resume latch was not cleared on the selected session"
                );
                stage_session_resume_in_txn(txn, identity.as_ref(), &node_did, &session_id).await
            })
        })
        .await
}

/// Core's resume rule, then `control.session.resume`: an interrupted or claimed
/// execution must settle before any plugin restages the session's work.
pub(crate) async fn stage_session_resume_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    identity: &dyn gents_store::identity::NodeIdentity,
    node_did: &str,
    session_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        !session_request_unsettled_in_txn(txn, node_did, session_id).await?,
        "session work is still settling; retry resume after the interrupted request terminates"
    );
    crate::request::hooks::points::session_resume(txn, identity, node_did, session_id).await
}

/// Whether any request of the session is an execution that has not settled
/// (claimed, running or interrupted and not yet terminal).
async fn session_request_unsettled_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<bool> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(node_did),
            gents_store::graphql::escape_graphql_string(session_id),
        ))
        .await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("request query omitted rows")?;
    for row in rows {
        let state: Option<gents_protocol::request_lifecycle::RequestLifecycleState> =
            serde_json::from_value(
                row.get("lifecycle_state")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )?;
        if state.is_some_and(
            gents_protocol::request_lifecycle::RequestLifecycleState::is_unsettled_execution,
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn ensure_session_executions_settled_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    executions: &Arc<dyn SessionExecutionWitness>,
) -> Result<()> {
    let scope = gents_protocol::session::session_scope_filter(node_did, session_id, requester_did);
    let unsettled = session_tool_call_exists_in_txn(
        txn,
        &format!(
            r#"filter: {{ {scope}, lifecycle_state: {{ _nin: ["completed", "failed", "timedOut", "cancelled"] }} }}"#
        ),
    )
    .await?;
    anyhow::ensure!(!unsettled, "session-owned request or tool execution is still settling; retry agent_resume after agent_session_read reports completion");

    anyhow::ensure!(
        !session_doc_exists_in_txn(txn, &scope, executions.active_tool_call_doc_ids()).await?,
        "session-owned background execution is still active; retry agent_resume after it settles"
    );
    let witness = Arc::clone(executions);
    let unexited = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>(
            witness
                .process_records()?
                .into_iter()
                .filter(|record| {
                    record.identity.observe() != crate::exec::ownership::ProcessObservation::Exited
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
        !session_doc_exists_in_txn(txn, &scope, executions.in_flight_job_row_ids()).await?,
        "session-owned plugin job is still executing; retry agent_resume after its step settles"
    );
    Ok(())
}

async fn session_doc_exists_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    scope: &str,
    doc_ids: impl IntoIterator<Item = String>,
) -> Result<bool> {
    let doc_ids = doc_ids
        .into_iter()
        .map(|id| format!("\"{}\"", gents_store::graphql::escape_graphql_string(&id)))
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
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
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
