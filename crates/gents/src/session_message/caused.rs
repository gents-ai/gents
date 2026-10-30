use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::graphql::escape_graphql_string;

use super::commit::load_receipt;

pub(crate) enum CausedObservation {
    Bound(Box<gents_protocol::row::AgentRequestRow>),
    NotVisible,
    Unbound(&'static str),
}

pub(crate) async fn observe_caused_request(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &crate::tool_call_lifecycle::ToolCallLifecycle,
) -> Result<CausedObservation> {
    let tool_call_doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?;
    let Some(receipt) = load_receipt(
        node,
        tool_call_doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    else {
        return Ok(CausedObservation::Unbound(
            "the row has no receipt naming the request it caused",
        ));
    };
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
            _docID request_id node_did requester_did session_id lifecycle_state input
            request_hop caused_by_parent_request_doc_id caused_by_parent_tool_call_id
            caused_by_parent_tool_call_doc_id
        }} }}"#,
        escape_graphql_string(&receipt.request_doc_id),
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load the request a session-message row caused",
    )
    .await?;
    let Some(caused) = crate::graphql::first_row::<gents_protocol::row::AgentRequestRow>(
        &response,
        "AgentRequest",
    )?
    else {
        return Ok(CausedObservation::NotVisible);
    };
    let caller = lifecycle.node_did();
    let session_message = caused.requester_did.as_deref() == Some(caller)
        && caused.caused_by_parent_tool_call_doc_id.as_deref() == Some(tool_call_doc_id)
        && caused.caused_by_parent_tool_call_id.as_deref() == Some(lifecycle.tool_call_id())
        && caused.caused_by_parent_request_doc_id.as_deref() == lifecycle.request_doc_id();
    if caused.request_id != receipt.request_id
        || caused.session_id.as_deref() != Some(receipt.session_id.as_str())
        || !session_message
    {
        tracing::warn!(
            tool_call_doc_id,
            caused_request_id = %caused.request_id,
            "caused request does not match its session-message receipt"
        );
        return Ok(CausedObservation::Unbound(
            "the receipt names a request that does not carry this call's lineage",
        ));
    }
    Ok(CausedObservation::Bound(Box::new(caused)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KillOutcome {
    Settled,
    Interrupting { request_id: String },
    Cancelled,
}

/// The caused-request step of a kill: `Some` when this step decides the kill
/// (the terminal request settled the row, or the live local request was
/// interrupted). `None` when the caller must cancel the row directly: a
/// terminal request this step did not settle, or a live remote request
/// (interrupted; a failed peer interrupt is only logged).
pub(crate) async fn kill_caused_request(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    caused: &gents_protocol::row::AgentRequestRow,
) -> Result<Option<KillOutcome>> {
    let terminal = caused
        .lifecycle_state
        .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal);
    if terminal && crate::background_completion::settle_session_message_row(node, lifecycle).await?
    {
        return Ok(Some(KillOutcome::Settled));
    }
    let doc_id = caused
        .doc_id
        .as_deref()
        .context("caused request lacks physical identity")?;
    let node_did = caused
        .node_did
        .as_deref()
        .context("caused request lacks node_did")?;
    let local = node_did == lifecycle.node_did();
    if !terminal {
        let interrupted = crate::interrupt::interrupt_request_by_doc_id(
            node,
            doc_id,
            node_did,
            caused.requester_did.as_deref(),
        )
        .await;
        match interrupted {
            Ok(_) if local => {
                return Ok(Some(KillOutcome::Interrupting {
                    request_id: caused.request_id.clone(),
                }))
            }
            Ok(_) => {}
            Err(error) if local => return Err(error),
            Err(error) => tracing::warn!(
                caused_request_doc_id = doc_id,
                error = %format!("{error:#}"),
                "peer interrupt of a killed session message was not written"
            ),
        }
    }
    Ok(None)
}

pub(crate) async fn kill(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
) -> Result<KillOutcome> {
    let caused = match observe_caused_request(node, lifecycle).await? {
        CausedObservation::Bound(caused) => Some(caused),
        CausedObservation::NotVisible | CausedObservation::Unbound(_) => None,
    };
    if let Some(caused) = &caused {
        if let Some(outcome) = kill_caused_request(node, lifecycle, caused).await? {
            return Ok(outcome);
        }
    }
    if lifecycle
        .cancel_during_run_owned(
            crate::tool_call_lifecycle::CancelCause::UserCancelled,
            "explicit_cancel",
        )
        .await?
    {
        let calling_request_id = lifecycle.calling_request_id().await?;
        crate::background_completion::append_background_tool_completion(
            node,
            lifecycle.session_id(),
            &calling_request_id,
            lifecycle
                .doc_id()
                .context("session-message row lacks physical identity")?,
            lifecycle.tool_name(),
            "cancelled",
            "",
            Some("explicit_cancel"),
            crate::lifecycle::RequestHopCause::Continuation,
        )
        .await?;
    }
    Ok(KillOutcome::Cancelled)
}
