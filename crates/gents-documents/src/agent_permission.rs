use gents_protocol::operations::permission::{
    AgentPermission, AgentPermissionError, AgentPermissionInput, AgentPermissionReceipt,
    PermissionChange,
};
use gents_store::config_client::{
    AgentSelfConfigPermission, AgentSelfConfigPermissionError, ConfigAccess,
};

use crate::desired_state::{find_node_doc_id, grant_agent_catalog, revoke_agent_catalog};

fn passthrough(error: impl Into<anyhow::Error>) -> AgentPermissionError {
    AgentPermissionError::Passthrough(
        error
            .into()
            .reallocate_into_boxed_dyn_error_without_backtrace(),
    )
}

/// Grants or revokes one named permission of an Agent as the caller. The
/// write is the relation behind the permission; DefraDB authorizes it.
pub async fn change_agent_permission(
    access: &ConfigAccess,
    input: &AgentPermissionInput,
) -> Result<AgentPermissionReceipt, AgentPermissionError> {
    let (node_did, agent_did) = (input.node_did.as_str(), input.agent_did.as_str());
    identity::Did::new(agent_did).map_err(|error| {
        AgentPermissionError::InvalidAgentDid(agent_did.to_owned(), Box::new(error))
    })?;
    match input.permission {
        AgentPermission::SelfConfig => {
            let permission = match input.change {
                PermissionChange::Grant => AgentSelfConfigPermission::Grant,
                PermissionChange::Revoke => AgentSelfConfigPermission::Revoke,
            };
            access
                .set_agent_self_config_permission(node_did, agent_did, permission)
                .await
                .map_err(|error| match error {
                    AgentSelfConfigPermissionError::AgentUnavailable => {
                        AgentPermissionError::AgentUnavailable(agent_did.to_owned())
                    }
                    error => passthrough(error),
                })?;
        }
        AgentPermission::Catalog => {
            let change = input.change;
            let known: anyhow::Result<bool> = access
                .transact_write("config.agent.catalog_permission", |txn| {
                    Box::pin(async move {
                        if find_node_doc_id(txn, node_did).await?.is_none() {
                            return Ok(false);
                        }
                        match change {
                            PermissionChange::Grant => {
                                grant_agent_catalog(txn, node_did, agent_did).await?
                            }
                            PermissionChange::Revoke => {
                                revoke_agent_catalog(txn, node_did, agent_did).await?
                            }
                        }
                        Ok(true)
                    })
                })
                .await;
            if !known.map_err(passthrough)? {
                return Err(AgentPermissionError::NodeUnavailable(node_did.to_owned()));
            }
        }
    }
    Ok(AgentPermissionReceipt {
        node_did: input.node_did.clone(),
        agent_did: input.agent_did.clone(),
        permission: input.permission,
        granted: input.change == PermissionChange::Grant,
    })
}
