use std::time::Duration;

use anyhow::{anyhow, Context as _};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::health_checker::HealthStatus;

use super::shared::{
    enforce_health_gate, extract_text, lookup_service, MetaToolContext, MetaToolError,
};

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

impl CallToolTool {
    pub(crate) fn new(ctx: MetaToolContext) -> Self {
        Self { ctx }
    }
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
