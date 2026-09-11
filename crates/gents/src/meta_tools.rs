use std::sync::Arc;

use defra_node::EmbeddedNode;

use crate::health_checker::ServiceHealthMap;
use crate::mcp_pool::McpPool;

mod call;
mod describe;
mod discover;
mod flat;
mod shared;
pub(crate) use flat::{flat_tool_name, presented_tool_names, selected_remote_identity};
#[cfg(test)]
mod tests;

pub(crate) use call::selected_tool_identity;
pub use call::{CallToolArgs, CallToolTool};
pub use describe::{DescribeToolArgs, DescribeToolTool};
pub use discover::{DiscoverToolsArgs, DiscoverToolsTool};
pub use shared::{MetaToolContext, MetaToolError};

pub const META_TOOL_NAMES: [&str; 3] = ["discover_tools", "describe_tool", "call_tool"];

pub async fn build_meta_tools(
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    agent_did: String,
    allowed_mcp_service_ids: Vec<String>,
    remote_tools: crate::document_config::RemoteTools,
) -> anyhow::Result<Vec<Box<dyn crate::llm::tool::ToolDyn>>> {
    let ctx = MetaToolContext {
        node,
        mcp_pool: mcp_pool.for_agent(&agent_did),
        health,
        local_hostname,
        local_subnet,
        agent_did,
        allowed_mcp_service_ids,
        remote_tools,
    };
    let mut tools: Vec<Box<dyn crate::llm::tool::ToolDyn>> = Vec::new();
    let mut discovery = false;
    for selection in &ctx.remote_tools.services {
        if selection.tool_names.is_empty()
            || ctx.service_selection(&selection.mcp_service_id).is_none()
        {
            continue;
        }
        if selection.style == crate::document_config::RemoteToolStyle::Discovery {
            discovery = true;
            continue;
        }
        let catalog = async {
            shared::enforce_health_gate(&ctx.health, &selection.mcp_service_id).await?;
            let service = shared::lookup_service(&ctx, &selection.mcp_service_id).await?;
            ctx.list_tools(&selection.mcp_service_id, &service).await
        }
        .await;
        let catalog = match catalog {
            Ok(catalog) => catalog,
            Err(error) if !selection.required => {
                tracing::warn!(service_id = %selection.mcp_service_id, error = %error, "optional flat MCP catalog unavailable");
                continue;
            }
            Err(error) => return Err(error),
        };
        for tool in catalog.tools {
            if !ctx.is_tool_allowed(&selection.mcp_service_id, tool.name.as_ref()) {
                continue;
            }
            let name = flat_tool_name(&selection.mcp_service_id, tool.name.as_ref());
            anyhow::ensure!(
                !tools.iter().any(|tool| tool.name() == name),
                "duplicate flat MCP tool {name}"
            );
            tools.push(Box::new(flat::FlatRemoteTool {
                definition: crate::llm::tool::ToolDefinition {
                    name,
                    description: format!(
                        "{} (service {}, tool {})",
                        tool.description.as_deref().unwrap_or_default(),
                        selection.mcp_service_id,
                        tool.name
                    ),
                    parameters: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
                },
                service_id: selection.mcp_service_id.clone(),
                tool_name: tool.name.to_string(),
                context: ctx.clone(),
            }));
        }
    }
    if discovery {
        tools.push(Box::new(DiscoverToolsTool::new(ctx.clone())));
        tools.push(Box::new(DescribeToolTool::new(ctx.clone())));
        tools.push(Box::new(CallToolTool::new(ctx)));
    }
    Ok(tools)
}
