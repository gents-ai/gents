use crate::commands::mcp_health::load_mcp_services_with_health_for_agent;
use crate::error::BridgeError;
use std::collections::HashSet;
use std::path::PathBuf;

use gents::{BehaviorToolConfig, ToolCeiling};
use serde_json::{json, Value};
use tauri::State;

use crate::state::{current_core, require_agent_home, DesktopAppState};

#[tauri::command]
pub async fn desktop_tool_surface_explain(
    agent_did: String,
    behavior_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let snapshot = core.store().snapshot();

    let behavior = snapshot
        .behaviors
        .iter()
        .find(|row| row.behavior_id == behavior_id && row.agent_did == agent_did)
        .ok_or_else(|| format!("behavior {behavior_id} not found for {agent_did}"))?;

    let agent_home = require_agent_home(&state)?;
    let (ceiling, ceiling_source) = resolve_desktop_tool_ceiling(&agent_home)?;
    let mcp_services_online = load_mcp_services_with_health_for_agent(core.as_ref(), &agent_did)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?
        .iter()
        .any(|row| matches!(row.status.as_deref(), Some("healthy" | "stale")));
    let active_behavior_ids = snapshot
        .behaviors
        .iter()
        .filter(|row| row.agent_did == agent_did && row.enabled)
        .map(|row| row.behavior_id.clone())
        .collect::<HashSet<_>>();
    let datastore_tool_surfaces = gents::list_datastore_tool_surfaces(core.node(), &agent_did)
        .await
        .map_err(|error| {
            BridgeError::untyped(format!(
                "loading DatastoreToolSurface documents for {agent_did}: {error}"
            ))
        })?;
    let eth_tools = gents::list_eth_tools(core.node(), &agent_did)
        .await
        .map_err(|error| {
            BridgeError::untyped(format!(
                "loading EthTool documents for {agent_did}: {error}"
            ))
        })?;

    let context_id = behavior
        .context_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let context = match context_id.as_deref() {
        Some(context_id) => Some(
            snapshot
                .contexts
                .iter()
                .find(|row| row.context_id == context_id && row.agent_did == agent_did)
                .ok_or_else(|| format!("referenced AgentContext {context_id} is missing"))?,
        ),
        None => None,
    };
    let tools_id = context
        .and_then(|context| context.tools_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let (tools_source, config) = match tools_id.as_deref() {
        Some(tools_id) => {
            let document = snapshot
                .tools
                .iter()
                .find(|row| row.tools_id == tools_id && row.agent_did == agent_did)
                .ok_or_else(|| format!("referenced Tools {tools_id} is missing"))?;
            let config = BehaviorToolConfig::from_tools_document_with_surfaces(
                &behavior.behavior_id,
                document,
                &datastore_tool_surfaces,
                &eth_tools,
                &ceiling,
                Vec::new(),
            )
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
            ("document", config)
        }
        None => ("default_missing_tools_id", BehaviorToolConfig::meta_only()),
    };

    let explanation =
        config.explain_with_runtime(mcp_services_online, &agent_did, &active_behavior_ids);
    Ok(json!({
        "behaviorId": behavior.behavior_id,
        "enabled": behavior.enabled,
        "contextId": context_id,
        "toolsId": tools_id,
        "toolsSource": tools_source,
        "ceilingSource": ceiling_source,
        "mcpServicesOnline": mcp_services_online,
        "surface": serde_json::to_value(&explanation).map_err(|error| BridgeError::untyped(error.to_string()))?,
    }))
}

fn resolve_desktop_tool_ceiling(
    agent_home: &std::path::Path,
) -> Result<(ToolCeiling, &'static str), BridgeError> {
    #[derive(serde::Deserialize)]
    struct InitCeilingView {
        #[serde(default)]
        tool_ceiling: Option<String>,
        #[serde(default)]
        tool_root: Option<String>,
    }

    let Some(config) = std::fs::read(agent_home.join("init.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<InitCeilingView>(&bytes).ok())
    else {
        return Ok((ToolCeiling::meta_only(), "default_no_init_json"));
    };

    let tool_root = config
        .tool_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    match config
        .tool_ceiling
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("metaonly") | Some("meta_only") => Ok((ToolCeiling::meta_only(), "init_json")),
        Some("readonly") => Ok((
            match tool_root.as_deref() {
                Some(root) => ToolCeiling::readonly_at(PathBuf::from(root)),
                None => ToolCeiling::readonly(),
            },
            "init_json",
        )),
        Some("readwrite") => {
            let root = tool_root.ok_or_else(|| {
                "init.json has readwrite tool_ceiling but no tool_root".to_string()
            })?;
            Ok((ToolCeiling::readwrite(PathBuf::from(root)), "init_json"))
        }
        Some(other) => Err(format!("init.json has unrecognized tool_ceiling {other:?}").into()),
    }
}
