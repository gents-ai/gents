//! MCP server surface for `defra_query`.
//!
//! Exposes the read-only structured query as an MCP tool over streamable-HTTP,
//! mounted at `/mcp` on the running `defra-agent server`. External consumers
//! (e.g. a trace/eval pipeline) can call `defra_query` instead of hand-rolling
//! a GraphQL client. The tool delegates to the same
//! [`run_defra_query`](crate::commands::query::run_defra_query) helper the CLI
//! uses, so both surfaces share the filter rendering, the always-on
//! secret-field guard, and the collection scope.

use std::sync::Arc;

use defra_agent::defra_query::{CollectionScope, DefraQueryParams};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::commands::query::run_defra_query;

/// MCP-facing arguments for the `defra_query` tool. Mirrors `DefraQueryParams`
/// but derives `JsonSchema` so the MCP tool advertises a typed input contract.
#[derive(Debug, Deserialize, JsonSchema)]
struct McpQueryArgs {
    /// Collection (GraphQL type) to read, e.g. "AgentRequest".
    collection: String,
    /// Field names to return; at least one is required.
    #[serde(default)]
    fields: Vec<String>,
    /// Optional DefraDB filter object, e.g. {"status": {"_eq": "completed"}}.
    #[serde(default)]
    filter: Option<Value>,
    /// Maximum rows to return (default 50, capped at 1000).
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Clone)]
pub(crate) struct DefraQueryMcp {
    graphql: String,
    scope: CollectionScope,
    tool_router: ToolRouter<Self>,
}

impl DefraQueryMcp {
    fn new(graphql: String, scope: CollectionScope) -> Self {
        Self {
            graphql,
            scope,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl DefraQueryMcp {
    #[tool(
        description = "Read-only structured query over a DefraDB collection. Provide a collection name, the fields to return, an optional DefraDB filter object (operators _eq/_gt/_in/_and/_or/_not), and an optional limit. Returns JSON {collection, count, results}. Sensitive fields (e.g. inference backend API keys) are always blocked."
    )]
    async fn defra_query(
        &self,
        Parameters(args): Parameters<McpQueryArgs>,
    ) -> Result<String, ErrorData> {
        let params = DefraQueryParams {
            collection: args.collection,
            filter: args.filter,
            fields: args.fields,
            limit: args.limit,
        };
        let value = run_defra_query(&self.graphql, &params, &self.scope)
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        serde_json::to_string_pretty(&value)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DefraQueryMcp {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (InitializeResult) is #[non_exhaustive]; default then set.
        #[allow(clippy::field_reassign_with_default)]
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "defra-agent read-only query surface. Use the `defra_query` tool to read agent \
             collections (AgentRequest, AgentResponse, AgentMessage, AgentToolCall, \
             AgentSession, ...) as structured JSON."
                .to_string(),
        );
        info
    }
}

/// Build the streamable-HTTP MCP service to mount at `/mcp`. A fresh handler is
/// created per session; all share the same graphql endpoint and collection scope.
pub(crate) fn defra_query_mcp_service(
    graphql: String,
    scope: CollectionScope,
) -> StreamableHttpService<DefraQueryMcp, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(DefraQueryMcp::new(graphql.clone(), scope.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    )
}
