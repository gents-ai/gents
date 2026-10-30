use defra_node::EmbeddedNode;

use super::selection::CommandOutputLimits;

pub(crate) fn budget_for_tool(limits: &CommandOutputLimits, tool_name: &str) -> usize {
    let configured = if crate::toolset::is_bash_tool_name(tool_name) {
        limits.bash
    } else {
        limits.cli.get(tool_name).copied()
    };
    configured.unwrap_or(crate::toolset::DEFAULT_MAX_COMMAND_CHARS)
}

pub(crate) async fn configured_output_budget(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    tool_name: &str,
) -> usize {
    match load_command_output_limits(node, node_did, agent_id).await {
        Ok(limits) => budget_for_tool(&limits, tool_name),
        Err(error) => {
            tracing::debug!(
                node_did,
                agent_id,
                tool_name,
                error = %format!("{error:#}"),
                "output budget uses the default: agent tools did not resolve"
            );
            crate::toolset::DEFAULT_MAX_COMMAND_CHARS
        }
    }
}

async fn load_command_output_limits(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) -> anyhow::Result<CommandOutputLimits> {
    let owner = node_did.to_owned();
    let agent_id = agent_id.to_owned();
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "tool_surface.output_budget",
        move |txn| {
            let owner = owner.clone();
            let agent_id = agent_id.clone();
            Box::pin(async move {
                match crate::documents::load_agent_tools_in_txn(txn, &owner, &agent_id).await? {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unresolvable_configuration_uses_the_default() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        assert_eq!(
            configured_output_budget(&node, "did:key:absent", "missing", "bash").await,
            crate::toolset::DEFAULT_MAX_COMMAND_CHARS
        );
    }
}
