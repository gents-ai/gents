#[derive(Debug, thiserror::Error)]
pub(crate) enum UnattendedAgentAdmissionError {
    #[error("unattended admission actor does not match the selected Agent")]
    ActorMismatch,
    #[error("selected Agent configuration is unavailable")]
    AgentUnavailable,
    #[error("selected Agent is disabled")]
    AgentDisabled,
    #[error("selected Agent cannot invoke this Agent")]
    InvokeDenied,
    #[error("selected Agent invoke authority is unavailable")]
    InvokeUnavailable,
    #[error("selected Agent observation failed: {0}")]
    Observation(#[source] anyhow::Error),
}

/// Observe the selected Agent's current native invoke authority in the fresh
/// admission transaction. The caller authenticates the transaction actor;
/// already admitted work continues under its own native effect checks.
pub(crate) async fn observe_unattended_agent_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    selected: &str,
) -> std::result::Result<(), UnattendedAgentAdmissionError> {
    use UnattendedAgentAdmissionError as Error;
    if txn.actor_did() != Some(selected) {
        return Err(Error::ActorMismatch);
    }
    let (doc, value) = crate::store::config_client::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Agent,
        owner,
        selected,
    )
    .await
    .map_err(Error::Observation)?
    .ok_or(Error::AgentUnavailable)?;
    let agent: super::Agent =
        serde_json::from_value(value).map_err(|error| Error::Observation(error.into()))?;
    if !agent.enabled {
        return Err(Error::AgentDisabled);
    }
    let actor = identity::Did::new(selected).map_err(|error| Error::Observation(error.into()))?;
    let decisions = txn
        .observe_document_access(
            &actor,
            "Agent",
            &[doc],
            defra_node::DocumentAccessMode::Named {
                permission: "invoke".into(),
                node_permission: acp::nac::NodePermission::DocumentUpdate,
            },
        )
        .await
        .map_err(|error| Error::Observation(error.into()))?;
    match decisions.as_slice() {
        [defra_node::DocumentAccessDecision::Allowed] => Ok(()),
        [defra_node::DocumentAccessDecision::Denied] => Err(Error::InvokeDenied),
        _ => Err(Error::InvokeUnavailable),
    }
}

#[cfg(test)]
mod tests;
