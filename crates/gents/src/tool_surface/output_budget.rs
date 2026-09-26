//! Output budget of a host command tool, resolved from the owning behavior's
//! current Tools document when a result is presented.
//!
//! Presentation owners (interrupted-call delivery, background completion
//! notices) run after the dispatching behavior's in-memory tool surface may be
//! gone — across a restart or a reconfiguration — so they resolve the budget
//! from configuration instead of from the call. The mapping from a document to
//! budgets is `ResolvedToolSelection::command_output_limits`, the same one
//! foreground tools are built from.

use defra_node::EmbeddedNode;

use super::selection::CommandOutputLimits;

/// Budget for `tool_name` under a resolved selection: `host.bash` for the bash
/// tools (including their `spawn_process` runs), `host.cli[name]` for CLI
/// tools, and the default command budget for every other tool.
pub(crate) fn budget_for_tool(limits: &CommandOutputLimits, tool_name: &str) -> usize {
    let configured = if crate::toolset::is_bash_tool_name(tool_name) {
        limits.bash
    } else {
        limits.cli.get(tool_name).copied()
    };
    configured.unwrap_or(crate::toolset::DEFAULT_MAX_COMMAND_CHARS)
}

/// Budget for `tool_name` as configured now for `behavior_id` under its owner
/// `agent_did`. A behavior, context or Tools document that no longer resolves
/// falls back to the default: presentation must never fail on configuration.
pub(crate) async fn configured_output_budget(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    tool_name: &str,
) -> usize {
    match load_command_output_limits(node, agent_did, behavior_id).await {
        Ok(limits) => budget_for_tool(&limits, tool_name),
        Err(error) => {
            tracing::debug!(
                agent_did,
                behavior_id,
                tool_name,
                error = %format!("{error:#}"),
                "output budget uses the default: behavior tools did not resolve"
            );
            crate::toolset::DEFAULT_MAX_COMMAND_CHARS
        }
    }
}

async fn load_command_output_limits(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) -> anyhow::Result<CommandOutputLimits> {
    let owner = agent_did.to_owned();
    let behavior_id = behavior_id.to_owned();
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "tool_surface.output_budget",
        move |txn| {
            let owner = owner.clone();
            let behavior_id = behavior_id.clone();
            Box::pin(async move {
                match crate::document_config::load_behavior_tools_in_txn(txn, &owner, &behavior_id)
                    .await?
                {
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

    #[test]
    fn tool_names_map_to_their_host_group() {
        let limits = CommandOutputLimits {
            bash: Some(10),
            cli: [("git".to_owned(), 20)].into(),
        };
        assert_eq!(budget_for_tool(&limits, "bash"), 10);
        assert_eq!(budget_for_tool(&limits, "bash_unrestricted"), 10);
        assert_eq!(budget_for_tool(&limits, "git"), 20);
        for other in ["jq", "call_tool", "mcp__search__query", "spawn_process"] {
            assert_eq!(
                budget_for_tool(&limits, other),
                crate::toolset::DEFAULT_MAX_COMMAND_CHARS,
                "{other}"
            );
        }
        assert_eq!(
            budget_for_tool(&CommandOutputLimits::default(), "bash"),
            crate::toolset::DEFAULT_MAX_COMMAND_CHARS
        );
    }

    #[tokio::test]
    async fn unresolvable_configuration_uses_the_default() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        assert_eq!(
            configured_output_budget(&node, "did:key:absent", "missing", "bash").await,
            crate::toolset::DEFAULT_MAX_COMMAND_CHARS
        );
    }
}
