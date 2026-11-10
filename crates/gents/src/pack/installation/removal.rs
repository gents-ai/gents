use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};

use crate::documents::{Agent, AgentContext, Tools};
use crate::store::config_client::ConfigRead;
use crate::store::graphql::escape_graphql_string;

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
    for (agent, tools, reference, _) in selected_plugin_references(access, owner).await? {
        anyhow::ensure!(
            !removed.contains(&reference.plugin),
            "cannot remove plugin {}: agent {:?} still selects it through Tools {:?}; remove that reference first with `gents pack wire {} --agent {} --remove --home <node-home>`",
            reference.plugin, agent, tools, reference.plugin, agent
        );
    }
    Err(super::PackRemovalError::ReferenceInventoryUnavailable.into())
}

pub(crate) async fn ensure_replaced_plugins_match<R>(
    access: &R,
    owner: &str,
    replacements: &BTreeMap<String, String>,
) -> Result<Option<String>>
where
    R: ConfigRead + ?Sized,
{
    Ok(plugin_pin_conflicts(access, owner, replacements)
        .await?
        .first()
        .map(|conflict| {
            format!(
                "cannot replace plugin {} with {}: agent {:?} selects pin {} through Tools {:?}",
                conflict.plugin, conflict.replacement, conflict.agent, conflict.pin, conflict.tools,
            )
        }))
}

/// A Tools document that selects a plugin, as far as the reading actor can see.
#[derive(Debug, Clone)]
pub struct PluginSelection {
    pub plugin: String,
    pub agent: String,
    pub tools: String,
    /// The selecting Tools' `integrations` without the named plugins.
    pub integrations: serde_json::Value,
}

/// The selections of `plugins` the reading actor can see. ACP filters the
/// read, so an empty answer is not proof that nothing selects them
/// (Lean `Configuration.pluginRemovalObservedSafe`).
pub async fn visible_plugin_selections<R>(
    access: &R,
    owner: &str,
    plugins: &BTreeSet<String>,
) -> Result<Vec<PluginSelection>>
where
    R: ConfigRead + ?Sized,
{
    let mut selections = Vec::new();
    for (agent, tools, reference, integrations) in selected_plugin_references(access, owner).await?
    {
        if !plugins.contains(&reference.plugin) {
            continue;
        }
        let mut remaining = integrations;
        if let Some(entries) = remaining.plugins.as_mut() {
            entries.retain(|entry| !plugins.contains(&entry.plugin));
        }
        selections.push(PluginSelection {
            plugin: reference.plugin,
            agent,
            tools,
            integrations: serde_json::to_value(&remaining)?,
        });
    }
    Ok(selections)
}

/// A selection whose pin refuses a plugin replacement
/// (Lean `Configuration.pluginReplacementSafe`).
#[derive(Debug, Clone)]
pub struct PluginPinConflict {
    pub plugin: String,
    pub replacement: String,
    pub agent: String,
    pub tools: String,
    pub pin: String,
    /// The selecting Tools' `integrations` with every replaced plugin's pin
    /// moved to its replacement: the value that lets the replacement through.
    pub integrations: serde_json::Value,
}

/// Every selection pinned to a digest other than the one `replacements`
/// publishes for its plugin, in the order the references are read.
pub async fn plugin_pin_conflicts<R>(
    access: &R,
    owner: &str,
    replacements: &BTreeMap<String, String>,
) -> Result<Vec<PluginPinConflict>>
where
    R: ConfigRead + ?Sized,
{
    if replacements.is_empty() {
        return Ok(Vec::new());
    }
    let mut conflicts = Vec::new();
    for (agent, tools, reference, integrations) in selected_plugin_references(access, owner).await?
    {
        let (Some(replacement), Some(pin)) =
            (replacements.get(&reference.plugin), &reference.digest)
        else {
            continue;
        };
        if pin == replacement {
            continue;
        }
        let mut moved = integrations;
        for entry in moved.plugins.iter_mut().flatten() {
            if let Some(replacement) = replacements.get(&entry.plugin) {
                if entry.digest.is_some() {
                    entry.digest = Some(replacement.clone());
                }
            }
        }
        conflicts.push(PluginPinConflict {
            plugin: reference.plugin,
            replacement: replacement.clone(),
            agent,
            tools,
            pin: pin.clone(),
            integrations: serde_json::to_value(&moved)?,
        });
    }
    Ok(conflicts)
}

async fn selected_plugin_references<R>(
    access: &R,
    owner: &str,
) -> Result<
    Vec<(
        String,
        String,
        crate::documents::PluginToolRef,
        crate::documents::IntegrationTools,
    )>,
>
where
    R: ConfigRead + ?Sized,
{
    let owner = escape_graphql_string(owner);
    let response = access.execute_read(&format!(r#"{{
        Agent(filter: {{node_did: {{_eq: "{owner}"}}}}) {{agent_did creation_nonce node_did context_id inference_profile_id}}
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
            "cannot verify plugin removal: agent {:?} references missing context {context_id:?}", agent.agent_did
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
                agent.agent_did.clone(),
                tools.tools_id.clone(),
                reference.clone(),
                tools.integrations.clone().unwrap_or_default(),
            ));
        }
    }
    Ok(references)
}
