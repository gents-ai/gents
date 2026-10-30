use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use std::sync::Arc;

use crate::graphql::escape_graphql_string;
use crate::request::lifecycle::SessionMessageCause;

use super::plan::{Plan, PlannedWrite};

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub(crate) struct SessionMessageReceipt {
    pub ok: bool,
    pub session_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    pub await_mode: String,
    pub status: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CommitError {
    #[error(transparent)]
    Steer(#[from] crate::request::lifecycle::queue::SteerAdmissionRefusal),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl crate::config_client::TransactionFailure for CommitError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Other(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            Self::Steer(_) => None,
        }
    }
}

pub(crate) async fn commit(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> std::result::Result<SessionMessageReceipt, CommitError> {
    let error = match commit_once(node, cause, lifecycle, plan, report_delivery).await {
        Ok(receipt) => return Ok(receipt),
        Err(error) => error,
    };
    let doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?
        .to_owned();
    let durable = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    .context("session-message row disappeared after its commit failed")?;
    if durable.state() == crate::tool_call_lifecycle::ToolCallState::Pending {
        return Err(error);
    }
    let receipt = load_receipt(
        node,
        &doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    .filter(|_| durable.is_running())
    .with_context(|| format!("session-message commit outcome is unknown after: {error:#}"))?;
    *lifecycle = durable;
    Ok(receipt)
}

async fn commit_once(
    node: &std::sync::Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> std::result::Result<SessionMessageReceipt, CommitError> {
    let delivery = report_delivery.then(|| plan.delivery().as_str().to_owned());
    let tool_call_id = lifecycle.tool_call_id().to_owned();
    let receipt_for =
        move |enqueued: &crate::request::lifecycle::EnqueuedAgentRequest| SessionMessageReceipt {
            ok: true,
            session_id: enqueued.session_id.clone(),
            request_id: enqueued.request_id.clone(),
            request_doc_id: enqueued.doc_id.clone(),
            tool_call_id: tool_call_id.clone(),
            delivery: delivery.clone(),
            await_mode: "background".to_owned(),
            status: "running".to_owned(),
        };
    let binding = lifecycle.background_receipt_binding()?;
    let start = lifecycle.dispatch_start()?;
    let (start, binding, plan, receipt_for) = (&start, &binding, &plan, &receipt_for);
    let (actor, label) = match &plan.write {
        PlannedWrite::Request { .. } => (None, "session_message.commit_request"),
        PlannedWrite::Goal { .. } => (
            Some(
                ::identity::Did::new(cause.caller_node_did.clone())
                    .context("caller DID is not ACP-addressable")?,
            ),
            "session_message.commit_goal",
        ),
    };
    let (receipt, started_at) =
        crate::config_client::ConfigAccess::transact_local_write(node, actor, label, move |txn| {
            Box::pin(async move {
                let started_at = start_dispatch(txn, start).await?;
                let enqueued = stage_planned_write_in_txn(txn, cause, plan).await?;
                let receipt = receipt_for(&enqueued);
                crate::tool_call_lifecycle::publish_background_receipt_in_txn(
                    txn,
                    binding,
                    &serde_json::to_string(&receipt).map_err(anyhow::Error::from)?,
                )
                .await?;
                Ok::<_, CommitError>((receipt, started_at))
            })
        })
        .await?;
    lifecycle.mark_started(started_at);
    Ok(receipt)
}

/// Create the planned request, or stage the Goal with it, in the caller's
/// transaction. It writes no tool row.
pub(crate) async fn stage_planned_write_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    cause: &SessionMessageCause,
    plan: &Plan,
) -> std::result::Result<crate::request::lifecycle::EnqueuedAgentRequest, CommitError> {
    if plan.interrupt_before_publish {
        let create = match &plan.write {
            PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
        };
        let active = crate::request::interrupt::interrupt_active_session_in_txn(
            txn,
            &create.session_id,
            &create.node_did,
            Some(create.requester_did.as_str()),
        )
        .await?;
        if active.is_none() && create.node_did == cause.caller_node_did {
            crate::goal::pause_goal_for_interrupt_in_txn(txn, &create.node_did, &create.session_id)
                .await?;
        }
    }
    match &plan.write {
        PlannedWrite::Request { create, .. } => {
            ensure_planned_hop_current(txn, create).await?;
            let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
            let response = txn.execute(&mutation).await?;
            let doc_id = crate::graphql::created_doc_id(&response, "AgentRequest")
                .map_err(anyhow::Error::from)?;
            if let Some(expected_head) = &plan.steer_head {
                validate_steer_in_txn(txn, create, &doc_id, expected_head.as_deref()).await??;
            }
            Ok(crate::request::lifecycle::EnqueuedAgentRequest {
                doc_id,
                request_id: create.request_id.clone(),
                session_id: create.session_id.clone(),
            })
        }
        PlannedWrite::Goal {
            create,
            objective,
            token_budget,
        } => {
            ensure_planned_hop_current(txn, create).await?;
            let enqueued = crate::goal::stage_goal_backed_request_in_txn(
                txn,
                &create.node_did,
                &plan.session_id,
                objective,
                *token_budget,
                create,
            )
            .await?;
            if let Some(expected_head) = &plan.steer_head {
                validate_steer_in_txn(txn, create, &enqueued.doc_id, expected_head.as_deref())
                    .await??;
            }
            Ok(enqueued)
        }
    }
}

async fn validate_steer_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    candidate_doc_id: &str,
    expected_head: Option<&str>,
) -> Result<std::result::Result<(), crate::request::lifecycle::queue::SteerAdmissionRefusal>> {
    let response = txn.execute(&format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }},
            purpose: {{ _eq: "normal" }}, lifecycle_state: {{ _in: ["claimed", "processing"] }} }}, limit: 2) {{
            {} execution_lease_secs interrupt_requested_at
        }} }}"#,
        escape_graphql_string(&create.node_did),
        escape_graphql_string(&create.session_id),
        crate::request::admission::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple active requests in target session"
    );
    anyhow::ensure!(
        rows.first().and_then(|row| row.doc_id.as_deref()) == expected_head,
        "the target session changed while the steer was planned"
    );
    let Some(row) = rows.into_iter().next() else {
        return Ok(Ok(()));
    };
    let head = crate::request::watcher::AgentRequest::try_from(row)?;
    let pending = crate::request::lifecycle::queue::pending_with_new_candidate_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
        candidate_doc_id,
    )
    .await?;
    Ok(crate::request::lifecycle::queue::steer_reachable(
        &head,
        pending,
        candidate_doc_id,
        chrono::Utc::now(),
    ))
}

async fn ensure_planned_hop_current(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let current =
        crate::session::load_session_current_hop_in_txn(txn, &create.node_did, &create.session_id)
            .await?;
    anyhow::ensure!(
        create.request_hop >= current,
        "the target session moved to hop {current} while this message was planned"
    );
    Ok(())
}

async fn start_dispatch(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    start: &crate::tool_call_lifecycle::delivery::DispatchStart,
) -> Result<chrono::DateTime<chrono::Utc>> {
    crate::tool_call_lifecycle::delivery::start_running_in_txn(txn, start, None)
        .await?
        .context("session-message row was altered or is no longer pending")
}

pub(crate) async fn load_receipt(
    node: &std::sync::Arc<EmbeddedNode>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionMessageReceipt>> {
    let read = crate::tool_call_lifecycle::query::load_tool_call_read(
        &crate::config_client::ConfigAccess::local(node.clone()),
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    let Some(message) = read.result else {
        return Ok(None);
    };
    let text = crate::tool_call_lifecycle::query::render_tool_result(&message)?;
    Ok(serde_json::from_str::<SessionMessageReceipt>(&text)
        .ok()
        .filter(|receipt| receipt.ok))
}
