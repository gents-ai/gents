use anyhow::{anyhow, Context as _};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use super::shared::{enforce_health_gate, lookup_service, MetaToolContext, MetaToolError};

#[derive(Debug, Deserialize)]
pub struct DescribeToolArgs {
    service_id: String,
    tool_name: String,
}

#[derive(Clone)]
pub struct DescribeToolTool {
    ctx: MetaToolContext,
}

impl DescribeToolTool {
    pub(crate) fn new(ctx: MetaToolContext) -> Self {
        Self { ctx }
    }
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
