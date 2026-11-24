use anyhow::Result;

use crate::request::interrupt::SessionInterruptError;
use defra_node::Node as EmbeddedNode;

use std::sync::Arc;

use gents_protocol::session::SessionScope;
use gents_store::config_client::ConfigAccess;

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

const INTERRUPT_REFUSAL: &str = "only the session that started this session or a same-node agent for a Task-triggered session may interrupt it";

pub async fn interrupt_refusal(
    node: &Arc<EmbeddedNode>,
    caller_agent_did: &str,
    caller: &SessionScope,
    target: &SessionScope,
) -> Result<Option<String>> {
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(caller_agent_did)?);
    let verdict =
        crate::session::origin::verified_agent_new_creation(&access, target, caller).await?;
    let cause = verdict.is_ok().then_some(caller.session_id.clone());
    let same_node = caller.node_did == target.node_did;
    let task_triggered = if same_node && cause.is_none() {
        crate::session::origin::task_triggered_opening(&access, target).await?
    } else {
        false
    };
    if agent_interrupt_allowed(
        &caller.session_id,
        &target.session_id,
        cause.as_deref(),
        same_node,
        task_triggered,
    ) {
        return Ok(None);
    }
    Ok(Some(match verdict {
        Ok(_) => INTERRUPT_REFUSAL.to_owned(),
        Err(denial) => format!("{INTERRUPT_REFUSAL} ({denial})"),
    }))
}

pub async fn interrupt_session(
    node: &Arc<EmbeddedNode>,
    caller_agent_did: &str,
    target: &SessionScope,
) -> Result<Option<String>, SessionInterruptError> {
    let access = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(caller_agent_did).map_err(anyhow::Error::from)?,
    );
    interrupt_session_with_access(node, &access, target).await
}

/// Interrupts `target` as the caller (Lean `InterruptGeneration`): returns the
/// request the interrupt saw running, or a typed refusal when the caller may
/// not update the session.
pub async fn interrupt_session_with_access(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    target: &SessionScope,
) -> Result<Option<String>, SessionInterruptError> {
    if node.node_identity_did() != Some(target.node_did.as_str()) {
        return Err(anyhow::anyhow!(
            "remote session interrupt requires an authenticated target runtime route"
        )
        .into());
    }
    let interrupted = access
        .transact_write("session.interrupt", |txn| {
            Box::pin(async move {
                crate::request::interrupt::interrupt_session_in_txn(
                    txn,
                    &target.session_id,
                    &target.node_did,
                    target.requester_did.as_deref(),
                )
                .await
            })
        })
        .await?;
    interrupted?;
    Ok(
        crate::request::interrupt::active_session_request_with_access(
            access,
            &target.session_id,
            &target.node_did,
            target.requester_did.as_deref(),
        )
        .await?
        .map(|row| row.request_id),
    )
}

#[cfg(test)]
mod conformance {
    use super::agent_interrupt_allowed;

    #[test]
    fn interrupt_permission_matches_the_lean_model() {
        gents_lean_vocab::families::causal_hop_contracts_interrupt::replay(|i| {
            agent_interrupt_allowed(
                &i.caller_session,
                &i.target_session,
                i.target_origin_cause.as_deref(),
                i.same_node,
                i.task_triggered,
            )
        });
    }
}
