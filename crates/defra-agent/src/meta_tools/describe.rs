use rig::completion::ToolDefinition;
use rig::tool::Tool;
use rmcp::model::{ListToolsResult, Tool as McpTool};
use serde::Deserialize;

use super::shared::{
    enforce_health_gate, lookup_service, MetaToolContext, MetaToolError, StructuredToolError,
};

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
        if let Err(error) = enforce_health_gate(&self.ctx.health, &args.service_id).await {
            return Ok(StructuredToolError::service_unavailable(
                &args.service_id,
                &args.tool_name,
                format!(
                    "service '{}' is currently unavailable: {error:#}",
                    args.service_id
                ),
                true,
            )
            .to_result_text());
        }

        let endpoint = match lookup_service(&self.ctx, &args.service_id).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                return Ok(StructuredToolError::service_unavailable(
                    &args.service_id,
                    &args.tool_name,
                    format!(
                        "service '{}' is not available for describe_tool: {error:#}",
                        args.service_id
                    ),
                    false,
                )
                .to_result_text());
            }
        };

        let list_result = match self
            .ctx
            .mcp_pool
            .list_tools(&args.service_id, &endpoint)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                return Ok(StructuredToolError::service_unavailable(
                    &args.service_id,
                    &args.tool_name,
                    format!(
                        "service '{}' could not list tools for describe_tool: {error:#}",
                        args.service_id
                    ),
                    true,
                )
                .to_result_text());
            }
        };

        match describe_tool_result(&args.service_id, &args.tool_name, &list_result) {
            Ok(result) => Ok(result),
            Err(error) => Ok(error.to_result_text()),
        }
    }
}

fn describe_tool_result(
    service_id: &str,
    requested_tool_name: &str,
    list_result: &ListToolsResult,
) -> Result<String, StructuredToolError> {
    let tool = list_result
        .tools
        .iter()
        .find(|tool| tool.name == requested_tool_name);

    match tool {
        Some(tool) => Ok(format_tool_description(tool)),
        None => {
            let available_tools = list_result
                .tools
                .iter()
                .map(|tool| tool.name.to_string())
                .collect::<Vec<_>>();
            Err(StructuredToolError::describe_tool_not_found(
                service_id,
                requested_tool_name,
                available_tools,
            ))
        }
    }
}

fn format_tool_description(tool: &McpTool) -> String {
    let desc = tool.description.as_deref().unwrap_or("(no description)");
    let schema_json = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();

    format!(
        "## {name}\n{desc}\n\nInput schema:\n```json\n{schema_json}\n```",
        name = tool.name,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rmcp::model::{ListToolsResult, Tool as McpTool};
    use serde_json::{json, Value};

    use super::*;
    use crate::health_checker::ServiceHealthMap;
    use crate::mcp_pool::McpPool;

    fn x_data_search_tool() -> McpTool {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer" }
            },
            "required": ["query"]
        })
        .as_object()
        .expect("schema object")
        .clone();

        McpTool::new("search_posts", "Search x-data posts.", Arc::new(schema))
    }

    #[test]
    fn missing_x_data_tool_returns_structured_envelope_with_available_tools() {
        let list_result = ListToolsResult::with_all_items(vec![x_data_search_tool()]);
        let error = describe_tool_result("x-data", "search_post", &list_result)
            .expect_err("missing tool should return envelope");

        assert_eq!(error.failure_class, "tool_not_found");
        assert_eq!(error.path, "/tool_name");
        assert!(error.retryable);
        assert_eq!(error.service_id, "x-data");
        assert_eq!(error.tool_name, "search_post");
        assert_eq!(error.requested_tool_name.as_deref(), Some("search_post"));
        assert_eq!(
            error.available_tools,
            Some(vec!["search_posts".to_string()])
        );

        let value: Value = serde_json::from_str(&error.to_result_text()).expect("structured json");
        assert_eq!(value["ok"], false);
        assert_eq!(value["failure_class"], "tool_not_found");
        assert_eq!(value["requested_tool_name"], "search_post");
        assert_eq!(value["available_tools"], json!(["search_posts"]));
        assert!(value["message"]
            .as_str()
            .expect("message")
            .contains("available tools: search_posts"));
    }

    #[test]
    fn successful_describe_tool_output_format_stays_unchanged() {
        let expected_schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer" }
            },
            "required": ["query"]
        });
        let list_result = ListToolsResult::with_all_items(vec![x_data_search_tool()]);
        let output = describe_tool_result("x-data", "search_posts", &list_result)
            .expect("known tool should describe");

        assert!(
            output.starts_with("## search_posts\nSearch x-data posts.\n\nInput schema:\n```json\n")
        );
        assert!(output.ends_with("\n```"));
        let schema_text = output
            .split("```json\n")
            .nth(1)
            .and_then(|text| text.strip_suffix("\n```"))
            .expect("schema block");
        let schema: Value = serde_json::from_str(schema_text).expect("schema json");
        assert_eq!(schema, expected_schema);
    }

    #[tokio::test]
    async fn missing_service_returns_structured_envelope() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let tool = DescribeToolTool::new(MetaToolContext {
            node,
            mcp_pool: McpPool::new(),
            health: ServiceHealthMap::new(),
            local_hostname: "studio-1".to_string(),
            local_subnet: None,
        });

        let output = tool
            .call(DescribeToolArgs {
                service_id: "missing-service".to_string(),
                tool_name: "search_posts".to_string(),
            })
            .await
            .expect("missing service should be model-readable");
        let value: Value = serde_json::from_str(&output).expect("structured json");

        assert_eq!(value["ok"], false);
        assert_eq!(value["failure_class"], "service_unavailable");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["service_id"], "missing-service");
        assert_eq!(value["requested_tool_name"], "search_posts");
        assert_eq!(value["path"], "/service_id");
    }
}
