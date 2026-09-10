use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use gents::mcp_pool::{resolve_mcp_url, McpPool};
use gents_desktop_core::client::ClientCore;

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
    let mcp_path = require_trimmed("mcp_path", request.mcp_path.clone().unwrap_or_default())?;
    Ok(resolve_mcp_url(
        &hostname,
        &tailscale_ip,
        &lan_ip,
        mcp_port as u16,
        &mcp_path,
        "",
        None,
    ))
}

pub async fn save_tool_service_config(
    core: &ClientCore,
    request: ToolServiceSaveRequest,
) -> Result<()> {
    core.save_tool_service_registry(&request.document).await
}

pub async fn test_tool_service_config(
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
