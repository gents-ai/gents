use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use gents::tool_surface::{measured_mcp_services_for_access, RuntimeToolAvailability};
use gents::{BehaviorToolConfig, ToolCeiling};
use serde_json::{json, Value};

use crate::cli::args::{ToolCeilingArg, ToolExplainArgs, ToolsCommand};
use crate::shared::StoredInitConfig;
use crate::{
    build_config_export_bundle, format_tool_ceiling, print_json, read_init_config,
    resolve_agent_did, resolve_config_access,
};

pub(crate) async fn dispatch(command: ToolsCommand) -> Result<()> {
    match command {
        ToolsCommand::Explain(args) => explain(args).await,
    }
}

async fn explain(args: ToolExplainArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let init_config = read_init_config(&home_dir)?;
    let (ceiling_arg, ceiling_source, tool_root, tool_ceiling) =
        resolve_tool_ceiling(init_config.as_ref())?;
    let bundle = build_config_export_bundle(&access, &agent_did).await?;
    let available_services = measured_mcp_services_for_access(
        &access,
        &agent_did,
        &bundle.config.tool_service_registries,
    )
    .await?;
    let availability =
        RuntimeToolAvailability::from_online_mcp_services(available_services.clone());
    let enabled_behavior_ids = bundle
        .config
        .agent_behaviors
        .iter()
        .filter(|behavior| behavior.enabled)
        .map(|behavior| behavior.behavior_id.clone())
        .collect::<BTreeSet<_>>();
    let enabled_behavior_id_set = enabled_behavior_ids.iter().cloned().collect::<HashSet<_>>();
    let mut configuration_issues = BTreeMap::new();
    let mut behaviors = Vec::new();
    for behavior in &bundle.config.agent_behaviors {
        if args
            .behavior_id
            .as_deref()
            .is_some_and(|id| behavior.behavior_id != id)
        {
            continue;
        }
        if !behavior.enabled {
            configuration_issues.insert(
                behavior.behavior_id.clone(),
                "behavior is disabled".to_owned(),
            );
        }
        let resolved = (|| -> Result<_> {
            let context = behavior
                .context_id
                .as_deref()
                .map(|id| {
                    bundle
                        .config
                        .contexts
                        .iter()
                        .find(|context| context.agent_did == agent_did && context.context_id == id)
                        .with_context(|| format!("referenced AgentContext {id} is missing"))
                })
                .transpose()?;
            let tools_id = context.and_then(|context| context.tools_id.as_deref());
            let config = if let Some(id) = tools_id {
                let tools = bundle
                    .config
                    .tools
                    .iter()
                    .find(|tools| tools.agent_did == agent_did && tools.tools_id == id)
                    .with_context(|| format!("referenced Tools {id} is missing"))?;
                BehaviorToolConfig::from_tools_documents(
                    &behavior.behavior_id,
                    tools,
                    &bundle.config.datastore_tool_surfaces,
                    &bundle.config.eth_tools,
                    &bundle.config.subagent_targets,
                    &tool_ceiling,
                    Vec::new(),
                )?
            } else {
                BehaviorToolConfig::from_tools_documents(
                    &behavior.behavior_id,
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
                configuration_issues.insert(behavior.behavior_id.clone(), format!("{error:#}"));
                continue;
            }
        };
        let explanation = config.explain_with_runtime_availability(
            availability.clone(),
            &agent_did,
            &enabled_behavior_id_set,
        );
        behaviors.push(json!({
            "behavior_id": behavior.behavior_id,
            "display_name": behavior.display_name,
            "enabled": behavior.enabled,
            "context_id": behavior.context_id,
            "tools_id": tools_id,
            "tools_source": if tools_id.is_some() { "document" } else { "default_no_tools_binding" },
            "surface": explanation,
        }));
    }

    if let Some(only_behavior_id) = args.behavior_id.as_deref() {
        let found = behaviors.iter().any(|row| {
            row.get("behavior_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == only_behavior_id)
        }) || configuration_issues.contains_key(only_behavior_id);
        if !found {
            anyhow::bail!("behavior {only_behavior_id} was not found for agent {agent_did}");
        }
    }

    let output = json!({
        "agent_did": agent_did,
        "access_mode": access.mode(),
        "home": home_dir,
        "host_tool_ceiling": {
            "tool_ceiling": format_tool_ceiling(ceiling_arg),
            "source": ceiling_source,
            "tool_root": tool_root,
            "scope": "host_native_file_bash_cli_only",
            "note": "This ceiling currently clamps host-native file/bash/CLI tools, not every model-callable built-in read, MCP, subagent, or operator HTTP surface."
        },
        "runtime_availability": {
            "measured_available_mcp_service_ids": available_services,
            "local_target_eligibility": {
                "source": "enabled_behavior_configuration",
                "enabled_behavior_ids": enabled_behavior_ids.iter().cloned().collect::<Vec<_>>(),
                "runtime_readiness_verified": false
            },
        },
        "behaviors": behaviors,
        "configuration_issues": configuration_issues,
        "operator_surfaces": {
            "included_in_model_tool_surface": false,
            "note": "Server HTTP routes and optional external /mcp are binary/operator surfaces; they are not included in per-behavior model-callable tool_names."
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
