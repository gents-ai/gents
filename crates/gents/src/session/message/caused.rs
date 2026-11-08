use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

use crate::store::graphql::escape_graphql_string;

use super::commit::load_receipt;

pub(crate) enum CausedObservation {
    Bound(Box<gents_protocol::row::AgentRequestRow>),
    NotVisible,
    Unbound(&'static str),
}

pub(crate) async fn observe_caused_request(
    _node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &crate::tools::call::lifecycle::ToolCallLifecycle,
) -> Result<CausedObservation> {
    let tool_call_doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?;
    let access = lifecycle.access()?;
    let Some(receipt) = load_receipt::<crate::tools::call::lifecycle::ToolCallLifecycle>(
        &access,
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
    let response = access.execute(&query).await?;
    let Some(row) = response.pointer("/data/AgentRequest/0") else {
        return Ok(CausedObservation::NotVisible);
    };
    let caused: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())?;
    let caller = lifecycle.actor().to_string();
    let session_message = caused.requester_did.as_deref() == Some(caller.as_str())
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
    if caused.node_did.as_deref() != Some(lifecycle.node_did()) {
        // Lean `Recovery.observeCaused`: a session message causes requests
        // only on its calling runtime, so another runtime's binding fails closed.
        return Ok(CausedObservation::Unbound(
            "the receipt names a request bound to another runtime",
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

pub(crate) async fn kill(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &mut crate::tools::call::lifecycle::ToolCallLifecycle,
) -> Result<KillOutcome> {
    use crate::tools::background::job::control::{CausedFacts, KillAction, KillObservation};
    let caused = match observe_caused_request(node, lifecycle).await? {
        CausedObservation::Bound(caused) => Some(caused),
        CausedObservation::NotVisible | CausedObservation::Unbound(_) => None,
    };
    let observation = KillObservation::observe(caused.as_deref().map(|caused| {
        CausedFacts {
            terminal: caused
                .lifecycle_state
                .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal),
            this_runtime: caused.node_did.as_deref() == Some(lifecycle.node_did()),
        }
    }));
    match (observation.action(), caused.as_deref()) {
        (KillAction::Settle, _) => {
            if crate::tools::background::completion::settle_session_message_row(node, lifecycle)
                .await?
            {
                return Ok(KillOutcome::Settled);
            }
        }
        (KillAction::InterruptCaused, Some(caused)) => {
            crate::request::interrupt::interrupt_request_by_doc_id_with_access(
                &lifecycle.access()?,
                caused
                    .doc_id
                    .as_deref()
                    .context("caused request lacks physical identity")?,
                lifecycle.node_did(),
                caused.requester_did.as_deref(),
            )
            .await?;
            return Ok(KillOutcome::Interrupting {
                request_id: caused.request_id.clone(),
            });
        }
        (KillAction::InterruptCaused, None) | (KillAction::CancelRow, _) => {}
    }
    if lifecycle
        .cancel_during_run_owned(
            crate::tools::call::lifecycle::CancelCause::UserCancelled,
            "explicit_cancel",
        )
        .await?
    {
        let calling_request_id = lifecycle.calling_request_id().await?;
        crate::tools::background::completion::append_background_tool_completion(
            node,
            &lifecycle.access()?,
            lifecycle.session_id(),
            &calling_request_id,
            lifecycle
                .doc_id()
                .context("session-message row lacks physical identity")?,
            lifecycle.tool_name(),
            "cancelled",
            "",
            Some("explicit_cancel"),
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await?;
    }
    Ok(KillOutcome::Cancelled)
}
