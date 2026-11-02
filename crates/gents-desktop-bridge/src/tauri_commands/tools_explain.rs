use crate::commands::mcp_health::load_mcp_services_with_health_for_node;
use crate::error::BridgeError;
use crate::types::ToolSurfaceExplanationView;
use gents::AgentToolSurfaceConfig;
use rapidhash::RapidHashSet;
use tauri::State;

use crate::state::{current_core, require_node_home, DesktopAppState};

#[tauri::command]
pub async fn desktop_tool_surface_explain(
    node_did: String,
    agent_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<ToolSurfaceExplanationView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let snapshot = core.store().snapshot();

    let agent = snapshot
        .agents
        .iter()
        .find(|row| row.agent_id == agent_id && row.node_did == node_did)
        .ok_or_else(|| format!("agent {agent_id} not found for {node_did}"))?;

    let node_home = require_node_home(&state)?;
    let (ceiling, ceiling_source) = crate::snapshot::resolve_node_home_tool_ceiling(&node_home)
        .map_err(BridgeError::untyped)?;
    let mcp_services_online = load_mcp_services_with_health_for_node(core.as_ref(), &node_did)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?
        .iter()
        .any(|row| matches!(row.status.as_deref(), Some("healthy" | "stale")));
    let active_agent_ids = snapshot
        .agents
        .iter()
        .filter(|row| row.node_did == node_did && row.enabled)
        .map(|row| row.agent_id.clone())
        .collect::<RapidHashSet<_>>();
    let datastore_tool_surfaces = gents::list_datastore_tool_surfaces(
        &gents::ConfigAccess::local(core.node_arc()),
        &node_did,
    )
    .await
    .map_err(|error| {
        BridgeError::untyped(format!(
            "loading DatastoreToolSurface documents for {node_did}: {error}"
        ))
    })?;

    let context_id = agent
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
                .find(|row| row.context_id == context_id && row.node_did == node_did)
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
                .find(|row| row.tools_id == tools_id && row.node_did == node_did)
                .ok_or_else(|| format!("referenced Tools {tools_id} is missing"))?;
            let config = AgentToolSurfaceConfig::from_tools_document_with_surfaces(
                &agent.agent_id,
                document,
                &datastore_tool_surfaces,
                &ceiling,
                Vec::new(),
            )
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
            ("document", config)
        }
        None => (
            "default_missing_tools_id",
            AgentToolSurfaceConfig::meta_only(),
        ),
    };

    let explanation =
        config.explain_with_runtime(mcp_services_online, &node_did, &active_agent_ids);
    Ok(ToolSurfaceExplanationView {
        agent_id: agent.agent_id.clone(),
        enabled: agent.enabled,
        context_id,
        tools_id,
        tools_source: tools_source.into(),
        ceiling_source: ceiling_source.into(),
        mcp_services_online,
        surface: explanation,
    })
}
