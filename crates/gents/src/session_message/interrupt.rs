use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use std::sync::Arc;

use crate::config_client::ConfigAccess;
use crate::session_origin::SessionScope;

pub fn agent_interrupt_allowed(
    caller_session: &str,
    target_session: &str,
    target_origin_cause: Option<&str>,
    same_node: bool,
    task_triggered: bool,
) -> bool {
    target_session != caller_session
        && (target_origin_cause == Some(caller_session) || (same_node && task_triggered))
}

pub async fn interrupt_refusal(
    node: &Arc<EmbeddedNode>,
    caller: &SessionScope,
    target: &SessionScope,
) -> Result<Option<String>> {
    let started = crate::session_origin::verified_agent_new_creation(
        &ConfigAccess::local(node.clone()),
        target,
        caller,
    )
    .await?;
    let cause = started.then_some(caller.session_id.clone());
    let same_node = caller.node_did == target.node_did;
    let task_triggered = if same_node && cause.is_none() {
        crate::session_origin::task_triggered_opening(&ConfigAccess::local(node.clone()), target)
            .await?
    } else {
        false
    };
    Ok((!agent_interrupt_allowed(&caller.session_id, &target.session_id, cause.as_deref(), same_node, task_triggered))
        .then(|| "only the session that started this session or a same-node agent for a Task-triggered session may interrupt it".to_owned()))
}

pub async fn interrupt_session(
    node: &Arc<EmbeddedNode>,
    target: &SessionScope,
) -> Result<Option<String>> {
    if node.node_identity_did() == Some(target.node_did.as_str()) {
        let session_id = target.session_id.clone();
        let node_did = target.node_did.clone();
        let requester_did = target.requester_did.clone();
        return crate::config_client::ConfigAccess::transact_local_write(
            node,
            None,
            "session.interrupt",
            move |txn| {
                let session_id = session_id.clone();
                let node_did = node_did.clone();
                let requester_did = requester_did.clone();
                Box::pin(async move {
                    let active = crate::interrupt::interrupt_active_session_in_txn(
                        txn,
                        &session_id,
                        &node_did,
                        requester_did.as_deref(),
                    )
                    .await?;
                    if active.is_none() {
                        crate::goal::pause_goal_for_interrupt_in_txn(txn, &node_did, &session_id)
                            .await?;
                    }
                    Ok(active)
                })
            },
        )
        .await;
    }

    let Some(active) = crate::interrupt::active_session_request(
        node,
        &target.session_id,
        &target.node_did,
        target.requester_did.as_deref(),
    )
    .await?
    else {
        return Ok(None);
    };
    crate::interrupt::interrupt_request_by_doc_id(
        node,
        active
            .doc_id
            .as_deref()
            .context("active request lacks physical identity")?,
        &target.node_did,
        active.requester_did.as_deref(),
    )
    .await?;
    Ok(Some(active.request_id))
}
