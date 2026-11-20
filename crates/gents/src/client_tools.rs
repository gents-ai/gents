use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use gents_tools::mcp::pool::{resolve_mcp_url, McpPool};

pub struct ToolServiceTarget {
    pub service_id: String,
    pub hostname: Option<String>,
    pub tailscale_ip: Option<String>,
    pub lan_ip: Option<String>,
    pub mcp_port: Option<i64>,
    pub mcp_path: Option<String>,
}

#[derive(Debug)]
pub struct ToolServiceTest {
    pub service_id: String,
    pub endpoint: String,
    pub tools: Vec<(String, Option<String>)>,
}

fn require_trimmed(name: &str, value: &str) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("{name} is required");
    }
    Ok(value)
}

fn trimmed(value: &Option<String>) -> String {
    value.as_deref().unwrap_or_default().trim().to_string()
}

fn resolve_tool_service_endpoint(target: &ToolServiceTarget) -> Result<String> {
    let mcp_port = target
        .mcp_port
        .ok_or_else(|| anyhow!("mcp_port is required"))?;
    if !(1..=u16::MAX as i64).contains(&mcp_port) {
        bail!("mcp_port must be between 1 and 65535");
    }
    let hostname = trimmed(&target.hostname);
    let tailscale_ip = trimmed(&target.tailscale_ip);
    let lan_ip = trimmed(&target.lan_ip);
    if hostname.is_empty() && tailscale_ip.is_empty() && lan_ip.is_empty() {
        bail!("hostname, tailscale_ip, or lan_ip is required");
    }
    let mcp_path = require_trimmed("mcp_path", target.mcp_path.as_deref().unwrap_or_default())?;
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

pub async fn test_tool_service(target: &ToolServiceTarget) -> Result<ToolServiceTest> {
    let service_id = require_trimmed("service_id", &target.service_id)?;
    let endpoint = resolve_tool_service_endpoint(target)?;
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
        .map(|tool| {
            (
                tool.name.to_string(),
                tool.description.as_deref().map(str::to_owned),
            )
        })
        .collect();
    Ok(ToolServiceTest {
        service_id,
        endpoint,
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> ToolServiceTarget {
        ToolServiceTarget {
            service_id: "svc".into(),
            hostname: Some("host".into()),
            tailscale_ip: None,
            lan_ip: None,
            mcp_port: Some(8080),
            mcp_path: Some("/mcp".into()),
        }
    }

    async fn refusal(target: ToolServiceTarget) -> String {
        test_tool_service(&target).await.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn a_target_without_a_host_is_refused() {
        let blank = ToolServiceTarget {
            hostname: Some("  ".into()),
            ..target()
        };
        assert_eq!(
            refusal(blank).await,
            "hostname, tailscale_ip, or lan_ip is required"
        );
    }

    #[tokio::test]
    async fn a_port_out_of_range_is_refused() {
        for port in [0, 65536] {
            let bad = ToolServiceTarget {
                mcp_port: Some(port),
                ..target()
            };
            assert_eq!(refusal(bad).await, "mcp_port must be between 1 and 65535");
        }
        let none = ToolServiceTarget {
            mcp_port: None,
            ..target()
        };
        assert_eq!(refusal(none).await, "mcp_port is required");
    }

    #[tokio::test]
    async fn a_blank_service_id_is_refused() {
        let blank = ToolServiceTarget {
            service_id: " ".into(),
            ..target()
        };
        assert_eq!(refusal(blank).await, "service_id is required");
    }
}
