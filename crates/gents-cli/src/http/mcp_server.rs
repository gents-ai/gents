use std::sync::Arc;

use gents::defra_query::{CollectionScope, QueryParams};
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct McpQueryArgs {
    argv: Vec<String>,
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    options: serde_json::Map<String, Value>,
}

#[derive(Clone)]
pub(crate) struct DefraQueryMcp {
    graphql: String,
    scope: CollectionScope,
    write_collections: std::collections::BTreeSet<String>,
    tool_router: ToolRouter<Self>,
}

impl DefraQueryMcp {
    fn new(
        graphql: String,
        scope: CollectionScope,
        write_collections: std::collections::BTreeSet<String>,
    ) -> Self {
        let mut tool_router = Self::tool_router();
        if write_collections.is_empty() {
            tool_router.remove_route("write");
        }
        Self {
            graphql,
            scope,
            tool_router,
            write_collections,
        }
    }
}

#[tool_router]
impl DefraQueryMcp {
    #[tool(
        description = "Read documents: argv:[fields], [find], [count], [search], [explain], [help,COMMAND]. Supply collection and options; count aggregates every matching row; find returns a bounded ordered page; search ranks keywords with native BM25. Explain inspects a native plan; executing it requires options.mode:execute."
    )]
    async fn query(&self, Parameters(args): Parameters<McpQueryArgs>) -> Result<String, ErrorData> {
        let params = QueryParams {
            argv: args.argv,
            collection: args.collection,
            options: args.options,
        };
        let access = gents::config_client::ConfigAccess::graphql(self.graphql.clone());
        let value=gents::defra_query::execute_command(&access,&params,&self.scope).await
            .map_err(|error|ErrorData::internal_error(serde_json::json!({"error":format!("{error:#}"),"recovery":{"tool":"query","args":{"argv":["help"]}}}).to_string(),None))?;
        gents::defra_query::render_result(value)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }
    #[tool(
        description = "Preview/apply application create/update/delete within an exact collection grant. argv:[help] gives syntax. Caller-signed DefraDB bearer required on every call; preview returns its bound next_call."
    )]
    async fn write(
        &self,
        Parameters(args): Parameters<McpQueryArgs>,
        ctx: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<String, ErrorData> {
        let parts = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .ok_or_else(|| {
                ErrorData::invalid_request("write requires HTTP caller authorization", None)
            })?;
        let authorization=parts.headers.get(axum::http::header::AUTHORIZATION).and_then(|h|h.to_str().ok()).ok_or_else(||ErrorData::invalid_request("write requires a caller-signed DefraDB Bearer authorization; query remains anonymous",None))?;
        let endpoint = gents::config_client::GraphqlEndpoint::with_delegated_authorization(
            self.graphql.clone(),
            authorization,
        )
        .map_err(|e| ErrorData::invalid_request(e.to_string(), None))?;
        let tool = gents::application_write::WriteTool::new(
            gents::config_client::ConfigAccess::Graphql(endpoint),
            self.write_collections.clone(),
            None,
        );
        let call = gents::application_write::WriteParams {
            argv: args.argv,
            collection: args.collection,
            options: args.options,
        };
        gents::llm::tool::Tool::call(&tool, call)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DefraQueryMcp {
    fn get_info(&self) -> ServerInfo {
        #[allow(clippy::field_reassign_with_default)]
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some("Read-only document access through query; call argv:[help] for commands. Writes, when explicitly exposed, require caller-signed DefraDB bearer authorization.".into());
        info
    }
}

pub(crate) fn defra_query_mcp_service(
    graphql: String,
    scope: CollectionScope,
    write_collections: std::collections::BTreeSet<String>,
) -> StreamableHttpService<DefraQueryMcp, LocalSessionManager> {
    StreamableHttpService::new(
        move || {
            Ok(DefraQueryMcp::new(
                graphql.clone(),
                scope.clone(),
                write_collections.clone(),
            ))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    )
}
