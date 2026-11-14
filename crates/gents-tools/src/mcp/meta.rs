use std::sync::Arc;

use defra_node::Node as EmbeddedNode;

use crate::mcp::health::ServiceHealthMap;
use crate::mcp::pool::McpPool;

mod call;
mod describe;
mod discover;
mod flat;
mod shared;
pub(crate) use flat::presented_tool_names;
pub use flat::{flat_tool_name, selected_remote_identity};
#[cfg(test)]
mod native_policy_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use call::selected_tool_identity;
pub use call::{CallToolArgs, CallToolTool};
pub use describe::{DescribeToolArgs, DescribeToolTool};
pub use discover::{DiscoverToolsArgs, DiscoverToolsTool};
pub use shared::{MetaToolContext, MetaToolError};

#[expect(
    clippy::too_many_arguments,
    reason = "each meta tool context input comes from a separate runtime owner"
)]
pub async fn build_meta_tools(
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    node_did: String,
    actor: identity::Did,
    allowed_mcp_service_ids: Vec<String>,
    remote_tools: gents_documents::RemoteTools,
    configuration: gents_inference::resolved::ResolvedConfiguration,
) -> anyhow::Result<Vec<Box<dyn gents_loop::tool::ToolDyn>>> {
    let mcp_pool = mcp_pool.for_agent(actor.as_str());
    let ctx = MetaToolContext {
        actor,
        node,
        mcp_pool,
        health,
        local_hostname,
        local_subnet,
        node_did,
        allowed_mcp_service_ids,
        remote_tools,
        configuration,
    };
    let mut tools: Vec<Box<dyn gents_loop::tool::ToolDyn>> = Vec::new();
    let mut discovery = false;
    for selection in &ctx.remote_tools.services {
        let Some(selection) = ctx.service_selection(&selection.mcp_service_id) else {
            continue;
        };
        if selection.style == gents_documents::RemoteToolStyle::Discovery {
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
                definition: gents_loop::tool::ToolDefinition {
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
