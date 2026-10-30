use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};

use crate::config_client::ConfigRead;
use crate::documents::{Agent, AgentContext, Tools};
use crate::graphql::escape_graphql_string;

/// Check the surviving document graph before retiring plugin files. Pack
/// removal calls this inside its transaction after proposed document deletions;
/// standalone plugin removal calls it before changing the local store. Missing
/// or malformed reads must refuse removal, never establish non-use.
pub async fn ensure_plugins_unreferenced<R>(
    access: &R,
    owner: &str,
    removed: &BTreeSet<String>,
) -> Result<()>
where
    R: ConfigRead + ?Sized,
{
    if removed.is_empty() {
        return Ok(());
    }
    for (agent, tools, reference) in selected_plugin_references(access, owner).await? {
        anyhow::ensure!(
            !removed.contains(&reference.plugin),
            "cannot remove plugin {}: agent {:?} still selects it through Tools {:?}; remove that reference first with `gents pack wire {} --agent {} --remove --home <node-home>`",
            reference.plugin, agent, tools, reference.plugin, agent
        );
    }
    Ok(())
}

pub(crate) async fn ensure_replaced_plugins_match<R>(
    access: &R,
    owner: &str,
    replacements: &BTreeMap<String, String>,
) -> Result<Option<String>>
where
    R: ConfigRead + ?Sized,
{
    if replacements.is_empty() {
        return Ok(None);
    }
    for (agent, tools, reference) in selected_plugin_references(access, owner).await? {
        if let (Some(replacement), Some(pin)) =
            (replacements.get(&reference.plugin), &reference.digest)
        {
            if pin != replacement {
                return Ok(Some(format!(
                    "cannot replace plugin {} with {}: agent {:?} selects pin {} through Tools {:?}",
                    reference.plugin, replacement, agent, pin, tools,
                )));
            }
        }
    }
    Ok(None)
}

async fn selected_plugin_references<R>(
    access: &R,
    owner: &str,
) -> Result<Vec<(String, String, crate::documents::PluginToolRef)>>
where
    R: ConfigRead + ?Sized,
{
    let owner = escape_graphql_string(owner);
    let response = access.execute_read(&format!(r#"{{
        Agent(filter: {{node_did: {{_eq: "{owner}"}}}}) {{agent_id node_did context_id inference_profile_id}}
        AgentContext(filter: {{node_did: {{_eq: "{owner}"}}}}) {{context_id node_did tools_id}}
        Tools(filter: {{node_did: {{_eq: "{owner}"}}}}) {{tools_id node_did integrations}}
    }}"#)).await?;
    let agents: Vec<Agent> = serde_json::from_value(response["data"]["Agent"].clone())
        .context("reading agents before plugin removal")?;
    let contexts: Vec<AgentContext> =
        serde_json::from_value(response["data"]["AgentContext"].clone())
            .context("reading contexts before plugin removal")?;
    let tools: Vec<Tools> = serde_json::from_value(response["data"]["Tools"].clone())
        .context("reading Tools before plugin removal")?;
    let context_count = contexts.len();
    let tool_count = tools.len();
    let contexts: BTreeMap<_, _> = contexts
        .iter()
        .map(|context| (&context.context_id, context))
        .collect();
    let tools: BTreeMap<_, _> = tools.iter().map(|tools| (&tools.tools_id, tools)).collect();
    anyhow::ensure!(
        contexts.len() == context_count,
        "ambiguous context IDs prevent plugin removal"
    );
    anyhow::ensure!(
        tools.len() == tool_count,
        "ambiguous Tools IDs prevent plugin removal"
    );
    let mut references = Vec::new();
    for agent in agents {
        let Some(context_id) = agent.context_id.as_ref() else {
            continue;
        };
        let context = contexts.get(context_id).with_context(|| format!(
            "cannot verify plugin removal: agent {:?} references missing context {context_id:?}", agent.agent_id
        ))?;
        let Some(tools_id) = context.tools_id.as_ref() else {
            continue;
        };
        let tools = tools.get(tools_id).with_context(|| format!(
            "cannot verify plugin removal: context {context_id:?} references missing Tools {tools_id:?}"
        ))?;
        for reference in tools
            .integrations
            .as_ref()
            .and_then(|integrations| integrations.plugins.as_deref())
            .unwrap_or_default()
        {
            references.push((
                agent.agent_id.clone(),
                tools.tools_id.clone(),
                reference.clone(),
            ));
        }
    }
    Ok(references)
}
