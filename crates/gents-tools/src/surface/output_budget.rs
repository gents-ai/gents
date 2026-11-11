use defra_node::Node as EmbeddedNode;

use super::selection::CommandOutputLimits;

pub(crate) fn budget_for_tool(limits: &CommandOutputLimits, tool_name: &str) -> usize {
    let configured = if crate::native::is_bash_tool_name(tool_name) {
        limits.bash
    } else {
        limits.cli.get(tool_name).copied()
    };
    configured.unwrap_or(crate::native::DEFAULT_MAX_COMMAND_CHARS)
}

pub async fn configured_output_budget(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
    tool_name: &str,
) -> usize {
    match load_command_output_limits(node, node_did, agent_did).await {
        Ok(limits) => budget_for_tool(&limits, tool_name),
        Err(error) => {
            tracing::debug!(
                node_did,
                agent_did,
                tool_name,
                error = %format!("{error:#}"),
                "output budget uses the default: agent tools did not resolve"
            );
            crate::native::DEFAULT_MAX_COMMAND_CHARS
        }
    }
}

async fn load_command_output_limits(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
) -> anyhow::Result<CommandOutputLimits> {
    let owner = node_did.to_owned();
    let agent_did = agent_did.to_owned();
    gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(identity::Did::new(&agent_did)?),
        "tool_surface.output_budget",
        move |txn| {
            let owner = owner.clone();
            let agent_did = agent_did.clone();
            Box::pin(async move {
                match gents_documents::load_agent_tools_in_txn(txn, &owner, &agent_did).await? {
                    Some(tools) => {
                        Ok(super::ResolvedToolSelection::from_document(&tools)?
                            .command_output_limits)
                    }
                    None => Ok(CommandOutputLimits::default()),
                }
            })
        },
    )
    .await
}
