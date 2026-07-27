use std::collections::HashSet;
use std::path::PathBuf;

use gents::{BehaviorToolConfig, ToolCeiling, ToolPolicyVersion, ToolSelection};
use serde_json::{json, Value};
use tauri::State;

use crate::state::{current_core, require_agent_home, DesktopAppState};

/// Resolved per-behavior tool surface, computed by the runtime's own
/// explain machinery — never re-derived outside it. Local agents only: a
/// remote agent's ceiling lives in its host's init config, which this
/// desktop cannot read.
#[tauri::command]
pub async fn desktop_tool_surface_explain(
    agent_did: String,
    behavior_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Value, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    if core.graphql_for_agent(&agent_did).await.is_some() {
        return Err("tool-surface explanation for remote agents is not yet supported".to_string());
    }
    let snapshot = core.store().snapshot();

    let behavior = snapshot
        .behaviors
        .iter()
        .find(|row| row.behavior_id == behavior_id && row.agent_did.as_deref() == Some(&agent_did))
        .ok_or_else(|| format!("behavior {behavior_id} not found for {agent_did}"))?;

    let agent_home = require_agent_home(&state)?;
    let (ceiling, ceiling_source) = resolve_desktop_tool_ceiling(&agent_home)?;
    let mcp_services_online = snapshot
        .tool_service_registries
        .iter()
        .any(|row| row.status.as_deref().unwrap_or("online") == "online");
    let active_behavior_ids = snapshot
        .behaviors
        .iter()
        .filter(|row| row.agent_did.as_deref() == Some(&agent_did) && row.enabled.unwrap_or(true))
        .map(|row| row.behavior_id.clone())
        .collect::<HashSet<_>>();

    let tool_selection_id = behavior
        .tool_selection_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let (tool_selection_source, config, tool_policy_version) = match tool_selection_id.as_deref() {
        Some(selection_id) => {
            let row = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == selection_id)
                .ok_or_else(|| format!("referenced ToolSelection {selection_id} is missing"))?;
            let document: gents::ToolSelectionDocument = serde_json::to_value(row)
                .and_then(serde_json::from_value)
                .map_err(|error| format!("decoding ToolSelection {selection_id}: {error}"))?;
            let config = BehaviorToolConfig::from_tool_selection_document(
                &behavior.behavior_id,
                &document,
                &ceiling,
                Vec::new(),
            )
            .map_err(|error| error.to_string())?;
            ("document", config, document.tool_policy_version.clone())
        }
        None => (
            "default_missing_tool_selection_id",
            BehaviorToolConfig::from_selection(
                &behavior.behavior_id,
                ToolSelection::default(),
                &ceiling,
                Vec::new(),
            )
            .map_err(|error| error.to_string())?,
            None,
        ),
    };

    let explanation =
        config.explain_with_runtime(mcp_services_online, &agent_did, &active_behavior_ids);
    let tool_policy_semantics = match ToolPolicyVersion::parse(tool_policy_version.as_deref()) {
        Ok(ToolPolicyVersion::LegacyDefaults) => "legacy-permissive",
        Ok(ToolPolicyVersion::V1) => "tool-policy/v1",
        Err(_) => "unknown",
    };

    Ok(json!({
        "behaviorId": behavior.behavior_id,
        "enabled": behavior.enabled.unwrap_or(true),
        "toolSelectionId": tool_selection_id,
        "toolSelectionSource": tool_selection_source,
        "toolPolicyVersion": tool_policy_version,
        "toolPolicySemantics": tool_policy_semantics,
        "ceilingSource": ceiling_source,
        "mcpServicesOnline": mcp_services_online,
        "surface": serde_json::to_value(&explanation).map_err(|error| error.to_string())?,
    }))
}

/// Desktop mirror of the CLI's init-config ceiling resolution; the desktop
/// stores `tool_ceiling` as a free string in the agent home's init.json.
fn resolve_desktop_tool_ceiling(
    agent_home: &std::path::Path,
) -> Result<(ToolCeiling, &'static str), String> {
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
        Some(other) => Err(format!("init.json has unrecognized tool_ceiling {other:?}")),
    }
}
