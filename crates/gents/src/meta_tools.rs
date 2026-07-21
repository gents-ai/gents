use std::sync::Arc;

use defra_node::EmbeddedNode;

use crate::health_checker::ServiceHealthMap;
use crate::mcp_pool::McpPool;

mod call;
mod describe;
mod discover;
mod shared;
#[cfg(test)]
mod tests;

pub use call::{CallToolArgs, CallToolTool};
pub use describe::{DescribeToolArgs, DescribeToolTool};
pub use discover::{DiscoverToolsArgs, DiscoverToolsTool};
pub use shared::{MetaToolContext, MetaToolError};

pub const META_TOOL_NAMES: [&str; 3] = ["discover_tools", "describe_tool", "call_tool"];

pub fn build_meta_tools(
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    agent_did: String,
    allowed_mcp_service_ids: Vec<String>,
) -> Vec<Box<dyn crate::llm::tool::ToolDyn>> {
    let ctx = MetaToolContext {
        node,
        mcp_pool,
        health,
        local_hostname,
        local_subnet,
        agent_did,
        allowed_mcp_service_ids,
    };
    vec![
        Box::new(DiscoverToolsTool::new(ctx.clone())),
        Box::new(DescribeToolTool::new(ctx.clone())),
        Box::new(CallToolTool::new(ctx)),
    ]
}
