use anyhow::Result;
use gents_desktop_core::client::ClientCore;
use gents_desktop_core::tool_service::{test_tool_service, ToolServiceTarget};

use super::super::types::{
    ToolServiceSaveRequest, ToolServiceTestRequest, ToolServiceTestResult, ToolServiceToolView,
};

pub async fn save_tool_service_config(
    core: &ClientCore,
    request: ToolServiceSaveRequest,
) -> Result<()> {
    core.save_tool_service_registry(&request.document).await
}

pub async fn test_tool_service_config(
    request: ToolServiceTestRequest,
) -> Result<ToolServiceTestResult> {
    let test = test_tool_service(&ToolServiceTarget {
        service_id: request.service_id,
        hostname: request.hostname,
        tailscale_ip: request.tailscale_ip,
        lan_ip: request.lan_ip,
        mcp_port: request.mcp_port,
        mcp_path: request.mcp_path,
    })
    .await?;
    let tools = test
        .tools
        .into_iter()
        .map(|(name, description)| ToolServiceToolView { name, description })
        .collect::<Vec<_>>();
    Ok(ToolServiceTestResult {
        service_id: test.service_id,
        endpoint: test.endpoint,
        status: "ok".to_string(),
        tool_count: tools.len(),
        tools,
        error: None,
    })
}
