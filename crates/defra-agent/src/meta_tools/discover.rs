use anyhow::anyhow;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::mcp_pool::resolve_mcp_url;

use super::shared::{format_health_status, MetaToolContext, MetaToolError};

#[derive(Debug, Deserialize)]
pub struct DiscoverToolsArgs {
    #[serde(default)]
    query: Option<String>,
}

#[derive(Clone)]
pub struct DiscoverToolsTool {
    ctx: MetaToolContext,
}

impl DiscoverToolsTool {
    pub(crate) fn new(ctx: MetaToolContext) -> Self {
        Self { ctx }
    }
}

impl Tool for DiscoverToolsTool {
    const NAME: &'static str = "discover_tools";

    type Error = MetaToolError;
    type Args = DiscoverToolsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Browse or search available MCP data service tools. Returns a compact \
                index of services and their tools (name + one-line description). Call with \
                no query to list all services, or provide a search query to filter. Use \
                describe_tool to get the compact required/optional argument contract before \
                calling a tool; request raw_schema only when exact JSON Schema is needed. \
                Native direct tools such as file or bash tools are not data services and are \
                described by their own tool definitions."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional search query to filter services and tools."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let gql = r#"{
  ToolServiceRegistry(
    filter: { status: { _eq: "online" } },
    order: { updated_at: DESC }
  ) {
    service_id
    display_name
    description
    hostname
    tailscale_ip
    lan_ip
    mcp_port
    mcp_path
  }
}"#;

        let resp = self.ctx.node.execute(gql).await;
        if resp.has_errors() {
            return Err(anyhow!("discover_tools query failed: {:?}", resp.errors).into());
        }

        let services = match resp.data.as_ref() {
            Some(data) => match data.get("ToolServiceRegistry").and_then(|v| v.as_array()) {
                Some(arr) => arr.clone(),
                None => {
                    tracing::warn!(
                        "discover_tools: response missing ToolServiceRegistry array — \
                         registry collection may not exist yet"
                    );
                    Vec::new()
                }
            },
            None => {
                tracing::warn!("discover_tools: response contained no data field");
                Vec::new()
            }
        };

        if services.is_empty() {
            return Ok("No data services are currently online.".to_string());
        }

        let query_lower = args.query.as_deref().map(|q| q.to_lowercase());

        let mut out = String::new();
        let mut matched = 0usize;

        for svc in &services {
            let sid = svc
                .get("service_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let name = svc
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(sid);
            let desc = svc
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let hostname = svc.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
            let health = self.ctx.health.get(sid).await;

            let svc_hostname = svc.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
            let svc_tsip = svc
                .get("tailscale_ip")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let svc_lanip = svc.get("lan_ip").and_then(|v| v.as_str()).unwrap_or("");
            let svc_port = svc.get("mcp_port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let svc_path = svc
                .get("mcp_path")
                .and_then(|v| v.as_str())
                .unwrap_or("/mcp");
            let endpoint = if svc_port > 0 {
                Ok(resolve_mcp_url(
                    svc_hostname,
                    svc_tsip,
                    svc_lanip,
                    svc_port,
                    svc_path,
                    &self.ctx.local_hostname,
                    self.ctx.local_subnet.as_deref(),
                ))
            } else {
                Err(anyhow!("no mcp_port"))
            };
            let tool_names: Vec<(String, String)> = if let Ok(ep) = &endpoint {
                match self.ctx.mcp_pool.list_tools(sid, ep).await {
                    Ok(list) => list
                        .tools
                        .iter()
                        .map(|t| {
                            (
                                t.name.to_string(),
                                t.description.as_deref().unwrap_or("").to_string(),
                            )
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                }
            } else {
                Vec::new()
            };

            if let Some(ref q) = query_lower {
                let tool_text = tool_names
                    .iter()
                    .map(|(n, d)| format!("{} {}", n, d).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                let haystack = format!(
                    "{} {} {} {} {}",
                    sid.to_lowercase(),
                    name.to_lowercase(),
                    desc.to_lowercase(),
                    hostname.to_lowercase(),
                    tool_text,
                );
                if !q.split_whitespace().all(|word| haystack.contains(word)) {
                    continue;
                }
            }

            matched += 1;
            out.push_str(&format!("## {name} ({sid})\n"));
            out.push_str(&format!(
                "Status: {}\n",
                format_health_status(health.as_ref())
            ));
            out.push_str(&format!(
                "Host: {}\n",
                if hostname.is_empty() {
                    "unknown"
                } else {
                    hostname
                }
            ));
            if !desc.is_empty() {
                out.push_str(&format!("{desc}\n"));
            }
            out.push_str("\nTools:\n");

            for (tn, td) in &tool_names {
                out.push_str(&format!("  - {tn}: {td}\n"));
            }
            out.push_str(
                "Next: call describe_tool with this service_id and a tool_name before call_tool.\n",
            );
            out.push('\n');
        }

        if matched == 0 {
            Ok(format!(
                "No services matched query {:?}. {} service(s) are online.",
                args.query.as_deref().unwrap_or(""),
                services.len()
            ))
        } else {
            Ok(out)
        }
    }
}
