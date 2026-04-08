//! Meta-tools for dynamic tool discovery and invocation.
//!
//! These three tools let the LLM discover, inspect, and call data service
//! tools at runtime via MCP, replacing the old hardcoded tool registry.
//!
//! - `discover_tools` — browse or search the ToolServiceRegistry
//! - `describe_tool`  — get the full input schema for one tool
//! - `call_tool`      — invoke a tool on a data service via MCP

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use defra_node::EmbeddedNode;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::health_checker::{HealthStatus, ServiceHealth, ServiceHealthMap};
use crate::mcp_pool::resolve_mcp_url;
use crate::mcp_pool::McpPool;

// ---------------------------------------------------------------------------
// Shared context
// ---------------------------------------------------------------------------

/// Shared state for all three meta-tools.
#[derive(Clone)]
pub struct MetaToolContext {
    pub node: Arc<EmbeddedNode>,
    pub mcp_pool: McpPool,
    pub health: ServiceHealthMap,
    pub local_hostname: String,
    pub local_subnet: Option<String>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct MetaToolError(anyhow::Error);

impl std::fmt::Display for MetaToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for MetaToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for MetaToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Escape a string for safe embedding inside a GraphQL string literal.
fn escape_graphql(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryServiceEntry {
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    tailscale_ip: String,
    #[serde(default)]
    lan_ip: String,
    mcp_port: Option<u16>,
    #[serde(default = "default_mcp_path")]
    mcp_path: String,
}

fn default_mcp_path() -> String {
    "/mcp".to_string()
}

/// Look up a service's MCP endpoint from the ToolServiceRegistry.
fn lookup_service_query(service_id: &str) -> String {
    let sid = escape_graphql(service_id);
    format!(
        r#"{{
  ToolServiceRegistry(
    filter: {{
      service_id: {{ _eq: "{sid}" }},
      status: {{ _eq: "online" }}
    }},
    order: {{ updated_at: DESC }},
    limit: 1
  ) {{
    service_id
    display_name
    description
    hostname
    tailscale_ip
    lan_ip
    mcp_port
    mcp_path
  }}
}}"#
    )
}

async fn lookup_service(ctx: &MetaToolContext, service_id: &str) -> anyhow::Result<String> {
    let resp = ctx.node.execute(&lookup_service_query(service_id)).await;
    if resp.has_errors() {
        anyhow::bail!("lookup_service({service_id}): {:?}", resp.errors);
    }

    let entry = resp
        .data
        .as_ref()
        .and_then(|d| d.get("ToolServiceRegistry"))
        .cloned()
        .map(serde_json::from_value::<Vec<RegistryServiceEntry>>)
        .transpose()
        .context("parsing ToolServiceRegistry response")?
        .and_then(|mut entries| entries.drain(..).next())
        .ok_or_else(|| anyhow!("service '{service_id}' not found or offline"))?;

    let mcp_port = entry
        .mcp_port
        .filter(|port| *port != 0)
        .ok_or_else(|| anyhow!("service '{service_id}' is missing mcp_port in the registry"))?;

    if entry.hostname.is_empty() && entry.tailscale_ip.is_empty() && entry.lan_ip.is_empty() {
        return Err(anyhow!(
            "service '{service_id}' is missing hostname/tailscale_ip/lan_ip in the registry"
        ));
    }

    let endpoint = resolve_mcp_url(
        &entry.hostname,
        &entry.tailscale_ip,
        &entry.lan_ip,
        mcp_port,
        &entry.mcp_path,
        &ctx.local_hostname,
        ctx.local_subnet.as_deref(),
    );

    Ok(endpoint)
}

/// Extract text content from an MCP CallToolResult.
fn extract_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.raw.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_elapsed(last_seen: chrono::DateTime<chrono::Utc>) -> String {
    let seconds = chrono::Utc::now()
        .signed_duration_since(last_seen)
        .num_seconds()
        .max(0);

    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn format_health_status(health: Option<&ServiceHealth>) -> String {
    match health {
        Some(health) => match (&health.status, &health.last_error) {
            (HealthStatus::Unreachable, Some(error)) => format!(
                "{} (last seen {} ago, error: {})",
                health.status,
                format_elapsed(health.last_seen),
                error
            ),
            _ => format!(
                "{} (last seen {} ago)",
                health.status,
                format_elapsed(health.last_seen)
            ),
        },
        None => "unknown (awaiting first health check)".to_string(),
    }
}

async fn enforce_health_gate(
    health_map: &ServiceHealthMap,
    service_id: &str,
) -> anyhow::Result<Option<ServiceHealth>> {
    let health = health_map.get(service_id).await;
    if let Some(health) = &health {
        match health.status {
            HealthStatus::Unreachable => {
                let suffix = health
                    .last_error
                    .as_deref()
                    .map(|error| format!(" (last error: {error})"))
                    .unwrap_or_default();
                anyhow::bail!("service '{service_id}' is currently unreachable{suffix}");
            }
            HealthStatus::Stale => {
                tracing::warn!(
                    service_id = %service_id,
                    last_seen = %health.last_seen,
                    "service heartbeat is stale; attempting tool request anyway"
                );
            }
            HealthStatus::Healthy => {}
        }
    }

    Ok(health)
}

// ---------------------------------------------------------------------------
// Tool 1: discover_tools
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DiscoverToolsArgs {
    #[serde(default)]
    query: Option<String>,
}

#[derive(Clone)]
pub struct DiscoverToolsTool {
    ctx: MetaToolContext,
}

impl Tool for DiscoverToolsTool {
    const NAME: &'static str = "discover_tools";

    type Error = MetaToolError;
    type Args = DiscoverToolsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Browse or search available data service tools. Returns a compact \
                index of services and their tools (name + one-line description). Call with \
                no query to list all services, or provide a search query to filter. Use \
                describe_tool to get full input schema before calling a tool."
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
        // Query all online services. The `tools` embedded relation is not
        // queryable via GraphQL sub-selection in DefraDB (known limitation),
        // so we fetch service metadata here and get tool lists via MCP
        // list_tools when needed.
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

            // Fetch tool list from MCP for this service.
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

            // Client-side filter if a query was given.
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

// ---------------------------------------------------------------------------
// Tool 2: describe_tool
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DescribeToolArgs {
    service_id: String,
    tool_name: String,
}

#[derive(Clone)]
pub struct DescribeToolTool {
    ctx: MetaToolContext,
}

impl Tool for DescribeToolTool {
    const NAME: &'static str = "describe_tool";

    type Error = MetaToolError;
    type Args = DescribeToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Get the full input schema for a specific tool on a data service. \
                Use this before call_tool to understand the required arguments."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "service_id": {
                        "type": "string",
                        "description": "The service_id of the data service."
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "The name of the tool to describe."
                    }
                },
                "required": ["service_id", "tool_name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        enforce_health_gate(&self.ctx.health, &args.service_id)
            .await
            .context("describe_tool")?;

        let endpoint = lookup_service(&self.ctx, &args.service_id)
            .await
            .context("describe_tool")?;

        let list_result = self
            .ctx
            .mcp_pool
            .list_tools(&args.service_id, &endpoint)
            .await
            .context("listing tools from MCP server")?;

        let tool = list_result.tools.iter().find(|t| t.name == args.tool_name);

        match tool {
            Some(t) => {
                let desc = t.description.as_deref().unwrap_or("(no description)");
                let schema_json = serde_json::to_string_pretty(&t.input_schema).unwrap_or_default();

                Ok(format!(
                    "## {name}\n{desc}\n\nInput schema:\n```json\n{schema_json}\n```",
                    name = t.name,
                ))
            }
            None => {
                let available: Vec<&str> =
                    list_result.tools.iter().map(|t| t.name.as_ref()).collect();
                Err(anyhow!(
                    "tool '{}' not found on service '{}'. Available tools: {}",
                    args.tool_name,
                    args.service_id,
                    available.join(", ")
                )
                .into())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 3: call_tool
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CallToolArgs {
    service_id: String,
    tool_name: String,
    arguments: serde_json::Value,
}

#[derive(Clone)]
pub struct CallToolTool {
    ctx: MetaToolContext,
}

impl Tool for CallToolTool {
    const NAME: &'static str = "call_tool";

    type Error = MetaToolError;
    type Args = CallToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Invoke a tool on a data service via MCP. Use discover_tools to \
                find available services and tools, then describe_tool to get the input \
                schema, then call_tool with the correct arguments."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "service_id": {
                        "type": "string",
                        "description": "The service_id of the data service."
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "The name of the tool to invoke."
                    },
                    "arguments": {
                        "type": "object",
                        "description": "The arguments to pass to the tool (see describe_tool for schema)."
                    }
                },
                "required": ["service_id", "tool_name", "arguments"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Models sometimes double-serialize arguments as a JSON string instead
        // of an object. Unwrap it if needed.
        let arguments = match &args.arguments {
            serde_json::Value::String(s) => {
                serde_json::from_str(s).unwrap_or(args.arguments.clone())
            }
            other => other.clone(),
        };

        let health = enforce_health_gate(&self.ctx.health, &args.service_id)
            .await
            .context("call_tool")?;

        let endpoint = lookup_service(&self.ctx, &args.service_id)
            .await
            .context("call_tool")?;

        // Use a generous timeout for stale services (120s) — heavy tools
        // like detect_anomalies can take minutes. The old 5s was too aggressive.
        let timeout_secs = if matches!(health.as_ref().map(|h| h.status), Some(HealthStatus::Stale))
        {
            120
        } else {
            300
        };

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.ctx
                .mcp_pool
                .call_tool(&args.service_id, &endpoint, &args.tool_name, arguments),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "service '{}' timed out after {}s",
                args.service_id,
                timeout_secs
            )
        })?
        .context("MCP call_tool")?;

        if result.is_error == Some(true) {
            let error_text = extract_text(&result);
            return Err(anyhow!(
                "tool '{}' on service '{}' returned an error: {}",
                args.tool_name,
                args.service_id,
                if error_text.is_empty() {
                    "(no error message)".to_string()
                } else {
                    error_text
                }
            )
            .into());
        }

        let text = extract_text(&result);
        Ok(if text.is_empty() {
            "(tool returned no text content)".to_string()
        } else {
            text
        })
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build the three meta-tools as boxed `ToolDyn` values.
pub fn build_meta_tools(
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
) -> Vec<Box<dyn rig::tool::ToolDyn>> {
    let ctx = MetaToolContext {
        node,
        mcp_pool,
        health,
        local_hostname,
        local_subnet,
    };
    vec![
        Box::new(DiscoverToolsTool { ctx: ctx.clone() }),
        Box::new(DescribeToolTool { ctx: ctx.clone() }),
        Box::new(CallToolTool { ctx }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // escape_graphql
    // -----------------------------------------------------------------------

    #[test]
    fn escape_graphql_handles_quotes() {
        assert_eq!(escape_graphql(r#"say "hello""#), r#"say \"hello\""#);
    }

    #[test]
    fn escape_graphql_handles_backslashes() {
        assert_eq!(escape_graphql(r"path\to\file"), r"path\\to\\file");
    }

    #[test]
    fn escape_graphql_handles_newlines_and_tabs() {
        assert_eq!(escape_graphql("line1\nline2\ttab"), r"line1\nline2\ttab");
    }

    #[test]
    fn escape_graphql_handles_carriage_return() {
        assert_eq!(escape_graphql("cr\r"), r"cr\r");
    }

    #[test]
    fn escape_graphql_combined() {
        assert_eq!(escape_graphql("a\\b\"c\nd"), r#"a\\b\"c\nd"#);
    }

    #[test]
    fn lookup_service_query_prefers_latest_online_row() {
        let query = lookup_service_query("x-data");
        assert!(query.contains(r#"service_id: { _eq: "x-data" }"#));
        assert!(query.contains(r#"status: { _eq: "online" }"#));
        assert!(query.contains(r#"order: { updated_at: DESC }"#));
        assert!(query.contains("limit: 1"));
    }

    #[test]
    fn lookup_service_query_escapes_service_id() {
        let query = lookup_service_query("x\"data");
        assert!(query.contains(r#"service_id: { _eq: "x\"data" }"#));
    }

    // -----------------------------------------------------------------------
    // extract_text
    // -----------------------------------------------------------------------

    fn make_call_result(texts: &[&str]) -> rmcp::model::CallToolResult {
        use rmcp::model::CallToolResult;

        let content = texts
            .iter()
            .map(|t| rmcp::model::Content::text(*t))
            .collect();

        CallToolResult::success(content)
    }

    #[test]
    fn extract_text_empty_content() {
        let result = make_call_result(&[]);
        assert_eq!(extract_text(&result), "");
    }

    #[test]
    fn extract_text_single_item() {
        let result = make_call_result(&["hello world"]);
        assert_eq!(extract_text(&result), "hello world");
    }

    #[test]
    fn extract_text_multiple_items_joined_with_newline() {
        let result = make_call_result(&["first", "second", "third"]);
        assert_eq!(extract_text(&result), "first\nsecond\nthird");
    }
}
