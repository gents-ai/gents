use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use defra_agent::mcp_pool::{resolve_mcp_url, McpPool};
use defra_agent_desktop_core::client::ClientCore;
use defra_agent_protocol::row::ToolServiceRegistryRow;

use super::super::types::{
    ToolServiceSaveRequest, ToolServiceTestRequest, ToolServiceTestResult, ToolServiceToolView,
};
use super::util::{require_trimmed, trim_optional};

fn resolve_tool_service_endpoint(request: &ToolServiceTestRequest) -> Result<String> {
    let mcp_port = request
        .mcp_port
        .ok_or_else(|| anyhow!("mcp_port is required"))?;
    if !(1..=u16::MAX as i64).contains(&mcp_port) {
        bail!("mcp_port must be between 1 and 65535");
    }
    let hostname = trim_optional(request.hostname.clone()).unwrap_or_default();
    let tailscale_ip = trim_optional(request.tailscale_ip.clone()).unwrap_or_default();
    let lan_ip = trim_optional(request.lan_ip.clone()).unwrap_or_default();
    if hostname.is_empty() && tailscale_ip.is_empty() && lan_ip.is_empty() {
        bail!("hostname, tailscale_ip, or lan_ip is required");
    }
    Ok(resolve_mcp_url(
        &hostname,
        &tailscale_ip,
        &lan_ip,
        mcp_port as u16,
        request.mcp_path.as_deref().unwrap_or("/mcp"),
        "",
        None,
    ))
}

pub(crate) async fn save_tool_service_config(
    core: &ClientCore,
    request: ToolServiceSaveRequest,
) -> Result<()> {
    let service_id = require_trimmed("service_id", request.service_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = core.store().snapshot();
    let mut row = store
        .tool_service_registries
        .iter()
        .find(|row| row.service_id == service_id)
        .cloned()
        .unwrap_or_else(|| ToolServiceRegistryRow {
            service_id: service_id.clone(),
            display_name: None,
            description: None,
            hostname: None,
            tailscale_ip: None,
            lan_ip: None,
            mcp_port: None,
            mcp_path: Some("/mcp".to_string()),
            tools: Vec::new(),
            status: Some("online".to_string()),
            version: None,
            updated_at: None,
        });
    row.display_name = Some(display_name);
    row.description = trim_optional(request.description);
    row.hostname = trim_optional(request.hostname);
    row.tailscale_ip = trim_optional(request.tailscale_ip);
    row.lan_ip = trim_optional(request.lan_ip);
    row.mcp_port = request.mcp_port;
    row.mcp_path = trim_optional(request.mcp_path).or_else(|| Some("/mcp".to_string()));
    row.status = trim_optional(request.status)
        .or_else(|| row.status.clone())
        .or_else(|| Some("online".to_string()));
    core.save_tool_service_registry(&row).await?;
    Ok(())
}

pub(crate) async fn test_tool_service_config(
    request: ToolServiceTestRequest,
) -> Result<ToolServiceTestResult> {
    let service_id = require_trimmed("service_id", request.service_id.clone())?;
    let endpoint = resolve_tool_service_endpoint(&request)?;
    let pool = McpPool::new();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        pool.list_tools(&service_id, &endpoint),
    )
    .await
    .context("MCP list_tools timed out")??;
    let tools = result
        .tools
        .iter()
        .map(|tool| ToolServiceToolView {
            name: tool.name.to_string(),
            description: tool.description.as_deref().map(str::to_owned),
        })
        .collect::<Vec<_>>();
    Ok(ToolServiceTestResult {
        service_id,
        endpoint,
        status: "ok".to_string(),
        tool_count: tools.len(),
        tools,
        error: None,
    })
}
