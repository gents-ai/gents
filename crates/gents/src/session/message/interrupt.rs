use anyhow::Result;
use defra_node::Node as EmbeddedNode;

use std::sync::Arc;

use crate::session::origin::SessionScope;
use crate::store::config_client::ConfigAccess;

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
    caller_agent_did: &str,
    caller: &SessionScope,
    target: &SessionScope,
) -> Result<Option<String>> {
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(caller_agent_did)?);
    let started =
        crate::session::origin::verified_agent_new_creation(&access, target, caller).await?;
    let cause = started.then_some(caller.session_id.clone());
    let same_node = caller.node_did == target.node_did;
    let task_triggered = if same_node && cause.is_none() {
        crate::session::origin::task_triggered_opening(&access, target).await?
    } else {
        false
    };
    Ok((!agent_interrupt_allowed(&caller.session_id, &target.session_id, cause.as_deref(), same_node, task_triggered))
        .then(|| "only the session that started this session or a same-node agent for a Task-triggered session may interrupt it".to_owned()))
}

pub async fn interrupt_session(
    node: &Arc<EmbeddedNode>,
    caller_agent_did: &str,
    target: &SessionScope,
) -> Result<Option<String>> {
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(caller_agent_did)?);
    interrupt_session_with_access(node, &access, target).await
}

pub(crate) async fn interrupt_session_with_access(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    target: &SessionScope,
) -> Result<Option<String>> {
    anyhow::ensure!(
        node.node_identity_did() == Some(target.node_did.as_str()),
        "remote session interrupt requires an authenticated target runtime route"
    );
    let session_id = target.session_id.clone();
    let node_did = target.node_did.clone();
    let requester_did = target.requester_did.clone();
    access
        .transact_write("session.interrupt", move |txn| {
            let session_id = session_id.clone();
            let node_did = node_did.clone();
            let requester_did = requester_did.clone();
            Box::pin(async move {
                let active = crate::request::interrupt::interrupt_active_session_in_txn(
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
        })
        .await
}
