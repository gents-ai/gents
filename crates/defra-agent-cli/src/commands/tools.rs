use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use defra_agent::{
    AgentBehaviorDocument, BehaviorToolConfig, ToolCeiling, ToolSelection, ToolSelectionDocument,
};
use serde_json::{json, Value};

use crate::cli::args::{ToolCeilingArg, ToolExplainArgs, ToolsCommand};
use crate::shared::{ConfigExportBundle, StoredInitConfig};
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
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let init_config = read_init_config(&home_dir)?;
    let (ceiling_arg, ceiling_source, tool_root, tool_ceiling) =
        resolve_tool_ceiling(init_config.as_ref())?;
    let bundle = build_config_export_bundle(&access, &agent_did).await?;
    let mcp_services_online = has_online_mcp_services(&bundle);
    let active_behavior_ids = active_behavior_ids(&bundle);
    let active_behavior_id_set = active_behavior_ids.iter().cloned().collect::<HashSet<_>>();
    let selection_rows = tool_selection_rows(&bundle)?;
    let mut unavailable_behaviors =
        crate::commands::status::collect_unavailable_behaviors_from_bundle(&bundle);
    let mut behaviors = Vec::new();

    for row in &bundle.agent_behaviors {
        let behavior: AgentBehaviorDocument = serde_json::from_value(row.clone())
            .context("decoding AgentBehavior row for tool-surface explanation")?;
        if let Some(only_behavior_id) = args.behavior_id.as_deref() {
            if behavior.behavior_id != only_behavior_id {
                continue;
            }
        }

        if !behavior.enabled {
            unavailable_behaviors
                .entry(behavior.behavior_id.clone())
                .or_insert_with(|| "behavior is disabled".to_string());
        }

        let tool_selection_id = behavior
            .tool_selection_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let (tool_selection_source, config_result) = match tool_selection_id.as_deref() {
            Some(selection_id) => match selection_rows.get(selection_id) {
                Some(selection) => (
                    "document".to_string(),
                    BehaviorToolConfig::from_tool_selection_document(
                        &behavior.behavior_id,
                        selection,
                        &tool_ceiling,
                        Vec::new(),
                    )
                    .with_context(|| format!("decoding ToolSelection {selection_id}")),
                ),
                None => {
                    unavailable_behaviors
                        .entry(behavior.behavior_id.clone())
                        .or_insert_with(|| {
                            format!("referenced ToolSelection {selection_id} is missing")
                        });
                    continue;
                }
            },
            None => (
                "default_missing_tool_selection_id".to_string(),
                BehaviorToolConfig::from_selection(
                    &behavior.behavior_id,
                    ToolSelection::default(),
                    &tool_ceiling,
                    Vec::new(),
                ),
            ),
        };

        let config = match config_result {
            Ok(config) => config,
            Err(error) => {
                unavailable_behaviors
                    .entry(behavior.behavior_id.clone())
                    .or_insert_with(|| error.to_string());
                continue;
            }
        };
        let explanation =
            config.explain_with_runtime(mcp_services_online, &agent_did, &active_behavior_id_set);
        behaviors.push(json!({
            "behavior_id": behavior.behavior_id,
            "display_name": behavior.display_name,
            "enabled": behavior.enabled,
            "tool_selection_id": tool_selection_id,
            "tool_selection_source": tool_selection_source,
            "surface": explanation,
        }));
    }

    if let Some(only_behavior_id) = args.behavior_id.as_deref() {
        let found = behaviors.iter().any(|row| {
            row.get("behavior_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == only_behavior_id)
        }) || unavailable_behaviors.contains_key(only_behavior_id);
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
            "online_mcp_services_present": mcp_services_online,
            "active_behavior_ids": active_behavior_ids.iter().cloned().collect::<Vec<_>>(),
        },
        "behaviors": behaviors,
        "unavailable_behaviors": unavailable_behaviors,
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

fn has_online_mcp_services(bundle: &ConfigExportBundle) -> bool {
    bundle.tool_service_registries.iter().any(|row| {
        row.get("status")
            .and_then(Value::as_str)
            .unwrap_or("online")
            == "online"
    })
}

fn active_behavior_ids(bundle: &ConfigExportBundle) -> BTreeSet<String> {
    bundle
        .agent_behaviors
        .iter()
        .filter(|row| row.get("enabled").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|row| {
            row.get("behavior_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn tool_selection_rows(
    bundle: &ConfigExportBundle,
) -> Result<BTreeMap<String, ToolSelectionDocument>> {
    let mut rows = BTreeMap::new();
    for row in &bundle.tool_selections {
        let selection: ToolSelectionDocument = serde_json::from_value(row.clone())
            .context("decoding ToolSelection row for tool-surface explanation")?;
        rows.insert(selection.selection_id.clone(), selection);
    }
    Ok(rows)
}
