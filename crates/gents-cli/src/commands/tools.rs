use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use gents::tool_surface::{measured_mcp_services_for_access, RuntimeToolAvailability};
use gents::{AgentToolSurfaceConfig, ToolCeiling};
use serde_json::{json, Value};

use crate::cli::args::{ToolCeilingArg, ToolExplainArgs, ToolsCommand};
use crate::shared::StoredInitConfig;
use crate::{
    build_config_export_bundle, format_tool_ceiling, print_json, read_init_config,
    resolve_config_access, resolve_node_did,
};

pub(crate) async fn dispatch(command: ToolsCommand) -> Result<()> {
    match command {
        ToolsCommand::Explain(args) => explain(args).await,
    }
}

async fn explain(args: ToolExplainArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let init_config = read_init_config(&home_dir)?;
    let (ceiling_arg, ceiling_source, tool_root, tool_ceiling) =
        resolve_tool_ceiling(init_config.as_ref())?;
    let bundle = build_config_export_bundle(&access, &node_did).await?;
    let available_services = measured_mcp_services_for_access(
        &access,
        &node_did,
        &bundle.config.tool_service_registries,
    )
    .await?;
    let availability =
        RuntimeToolAvailability::from_online_mcp_services(available_services.clone());
    let enabled_agent_ids = bundle
        .config
        .agents
        .iter()
        .filter(|agent| agent.enabled)
        .map(|agent| agent.agent_id.clone())
        .collect::<BTreeSet<_>>();
    let enabled_agent_id_set = enabled_agent_ids.iter().cloned().collect::<HashSet<_>>();
    let mut configuration_issues = BTreeMap::new();
    let mut agents = Vec::new();
    for agent in &bundle.config.agents {
        if args
            .agent_id
            .as_deref()
            .is_some_and(|id| agent.agent_id != id)
        {
            continue;
        }
        if !agent.enabled {
            configuration_issues.insert(agent.agent_id.clone(), "agent is disabled".to_owned());
        }
        let resolved = (|| -> Result<_> {
            let context = agent
                .context_id
                .as_deref()
                .map(|id| {
                    bundle
                        .config
                        .contexts
                        .iter()
                        .find(|context| context.node_did == node_did && context.context_id == id)
                        .with_context(|| format!("referenced AgentContext {id} is missing"))
                })
                .transpose()?;
            let tools_id = context.and_then(|context| context.tools_id.as_deref());
            let config = if let Some(id) = tools_id {
                let tools = bundle
                    .config
                    .tools
                    .iter()
                    .find(|tools| tools.node_did == node_did && tools.tools_id == id)
                    .with_context(|| format!("referenced Tools {id} is missing"))?;
                AgentToolSurfaceConfig::from_tools_documents(
                    &agent.agent_id,
                    tools,
                    &bundle.config.datastore_tool_surfaces,
                    &bundle.config.eth_tools,
                    &bundle.config.agent_targets,
                    &tool_ceiling,
                    Vec::new(),
                )?
            } else {
                AgentToolSurfaceConfig::from_tools_documents(
                    &agent.agent_id,
                    &gents::document_config::Tools::default(),
                    &[],
                    &[],
                    &[],
                    &tool_ceiling,
                    Vec::new(),
                )?
            };
            Ok((tools_id, config))
        })();
        let (tools_id, config) = match resolved {
            Ok(value) => value,
            Err(error) => {
                configuration_issues.insert(agent.agent_id.clone(), format!("{error:#}"));
                continue;
            }
        };
        let explanation = config.explain_with_runtime_availability(
            availability.clone(),
            &node_did,
            &enabled_agent_id_set,
        );
        agents.push(json!({
            "agent_id": agent.agent_id,
            "display_name": agent.display_name,
            "enabled": agent.enabled,
            "context_id": agent.context_id,
            "tools_id": tools_id,
            "tools_source": if tools_id.is_some() { "document" } else { "default_no_tools_binding" },
            "surface": explanation,
        }));
    }

    if let Some(only_agent_id) = args.agent_id.as_deref() {
        let found = agents.iter().any(|row| {
            row.get("agent_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == only_agent_id)
        }) || configuration_issues.contains_key(only_agent_id);
        if !found {
            anyhow::bail!("agent {only_agent_id} was not found for agent {node_did}");
        }
    }

    let output = json!({
        "node_did": node_did,
        "access_mode": access.mode(),
        "home": home_dir,
        "host_tool_ceiling": {
            "tool_ceiling": format_tool_ceiling(ceiling_arg),
            "source": ceiling_source,
            "tool_root": tool_root,
            "scope": "host_native_file_bash_cli_only",
            "note": "This ceiling currently clamps host-native file/bash/CLI tools, not every model-callable built-in read, MCP, agent, or operator HTTP surface."
        },
        "runtime_availability": {
            "measured_available_mcp_service_ids": available_services,
            "local_target_eligibility": {
                "source": "enabled_agent_configuration",
                "enabled_agent_ids": enabled_agent_ids.iter().cloned().collect::<Vec<_>>(),
                "runtime_readiness_verified": false
            },
        },
        "agents": agents,
        "configuration_issues": configuration_issues,
        "operator_surfaces": {
            "included_in_model_tool_surface": false,
            "note": "Server HTTP routes and optional external /mcp are binary/operator surfaces; they are not included in per-agent model-callable tool_names."
        },
    });
    print_json(&output)?;
    Ok(())
}

fn resolve_tool_ceiling(
    init_config: Option<&StoredInitConfig>,
) -> Result<(ToolCeilingArg, &'static str, Option<String>, ToolCeiling)> {
    let Some(config) = init_config else {
        return Ok((
            ToolCeilingArg::MetaOnly,
            "default_no_init_json",
            None,
            ToolCeiling::meta_only(),
        ));
    };
    let tool_root = config.tool_root.clone();
    let ceiling = match config.tool_ceiling {
        ToolCeilingArg::MetaOnly => ToolCeiling::meta_only(),
        ToolCeilingArg::Readonly => match tool_root.as_deref() {
            Some(root) => ToolCeiling::readonly_at(PathBuf::from(root)),
            None => ToolCeiling::readonly(),
        },
        ToolCeilingArg::Readwrite => {
            let root = tool_root.as_deref().ok_or_else(|| {
                anyhow::anyhow!("init.json has readwrite tool_ceiling but no tool_root")
            })?;
            ToolCeiling::readwrite(PathBuf::from(root))
        }
    };
    Ok((config.tool_ceiling, "init_json", tool_root, ceiling))
}
