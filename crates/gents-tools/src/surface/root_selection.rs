use anyhow::{Context, Result};
use gents_documents::views::config::ContextsUsingTools;
use gents_documents::{load_agent_context_and_tools_in_txn, write_tools_document_in_txn, Tools};
use gents_protocol::operations::workspace_root::{
    retarget_allowed, AgentWorkspaceRoot, AgentWorkspaceRootError, EnabledUpdate,
    WorkspaceRootPublish,
};
use gents_protocol::operations::StringUpdate;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

pub async fn select_agent_workspace_root(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    root_path: &str,
) -> std::result::Result<AgentWorkspaceRoot, AgentWorkspaceRootError> {
    access
        .transact_readonly("workspace_root.agent_tools", |txn| {
            Box::pin(async move { read_unshared_agent_tools(txn, node_did, agent_did).await })
        })
        .await
        .map_err(|error| {
            AgentWorkspaceRootError::Other(
                error.reallocate_into_boxed_dyn_error_without_backtrace(),
            )
        })?;
    let published = super::publish_workspace_root(
        access,
        &WorkspaceRootPublish {
            node_did: node_did.to_owned(),
            root_path: root_path.to_owned(),
            display_name: StringUpdate::Omitted,
            enabled: EnabledUpdate::Enable,
            reader_agents: vec![agent_did.to_owned()],
        },
    )
    .await?;
    let root_path = published.root_path.as_str();
    let tools_id = access
        .transact_write("workspace_root.retarget_tools", |txn| {
            Box::pin(async move {
                let mut tools = read_unshared_agent_tools(txn, node_did, agent_did).await?;
                let host = tools.host.get_or_insert_with(Default::default);
                if host.root.as_deref() != Some(root_path) {
                    host.root = Some(root_path.to_owned());
                    write_tools_document_in_txn(
                        txn,
                        &tools,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(tools.tools_id)
            })
        })
        .await
        .map_err(|error| AgentWorkspaceRootError::Retarget {
            root_path: root_path.to_owned(),
            source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
        })?;
    Ok(AgentWorkspaceRoot {
        root_path: root_path.to_owned(),
        workspace_root_doc_id: published.doc_id,
        tools_id,
    })
}

async fn read_unshared_agent_tools(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
) -> Result<Tools> {
    let (context, tools) = load_agent_context_and_tools_in_txn(txn, node_did, agent_did).await?;
    context.context(AgentWorkspaceRootError::ContextMissing)?;
    let tools = tools.context(AgentWorkspaceRootError::ToolsMissing)?;
    let selecting_contexts = txn
        .read_view(&ContextsUsingTools {
            node_did: node_did.to_owned(),
            tools_id: tools.tools_id.clone(),
        })
        .await?
        .len();
    if !retarget_allowed(selecting_contexts) {
        return Err(AgentWorkspaceRootError::SharedTools {
            tools_id: tools.tools_id.clone(),
            selecting_contexts,
        }
        .into());
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    #[test]
    fn retarget_admission_matches_lean() {
        gents_lean_vocab::families::workspace_root_operations_retarget_allowed_cases::replay(|i| {
            gents_protocol::operations::workspace_root::retarget_allowed(
                i.selecting_contexts as usize,
            )
        });
    }
}
