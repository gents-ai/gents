use std::collections::BTreeSet;
use std::sync::Arc;

use gents_store::config_client::{ConfigClientError, GraphqlEndpoint};
use gents_tools::builtin::defra_query::{CollectionScope, QueryParams};
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
pub struct McpServiceOptions {
    pub scope: CollectionScope,
    pub write_collections: BTreeSet<String>,
}

#[derive(Clone)]
pub struct DefraQueryMcp {
    graphql: String,
    options: McpServiceOptions,
    tool_router: ToolRouter<Self>,
}

impl DefraQueryMcp {
    fn new(graphql: String, options: McpServiceOptions) -> Self {
        let mut tool_router = Self::tool_router();
        if options.write_collections.is_empty() {
            tool_router.remove_route("write");
        }
        Self {
            graphql,
            options,
            tool_router,
        }
    }
}

/// HttpAccess.lean `mcpQueryReadsAs`: the tools hold no principal of their
/// own and forward the bearer the caller presented, so native ACP decides for
/// that caller; a call without one is refused before any document access.
fn caller_endpoint(
    graphql: &str,
    authorization: Option<&str>,
) -> Option<Result<GraphqlEndpoint, ConfigClientError>> {
    authorization
        .map(|authorization| GraphqlEndpoint::with_delegated_authorization(graphql, authorization))
}

impl DefraQueryMcp {
    fn caller(
        &self,
        ctx: &rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<GraphqlEndpoint, ErrorData> {
        let authorization = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.headers.get(axum::http::header::AUTHORIZATION))
            .and_then(|header| header.to_str().ok());
        caller_endpoint(&self.graphql, authorization)
            .ok_or_else(|| {
                ErrorData::invalid_request(
                    "a caller-signed DefraDB Bearer authorization is required on every call",
                    None,
                )
            })?
            .map_err(|error| ErrorData::invalid_request(error.to_string(), None))
    }
}

#[tool_router]
impl DefraQueryMcp {
    #[tool(
        description = "Read documents as the caller: argv:[fields], [find], [count], [search], [explain], [help,COMMAND]. Caller-signed DefraDB bearer required on every call. Supply collection and options; count aggregates every matching row; find returns a bounded ordered page; search ranks keywords with native BM25. Explain inspects a native plan; executing it requires options.mode:execute."
    )]
    async fn query(
        &self,
        Parameters(args): Parameters<McpQueryArgs>,
        ctx: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<String, ErrorData> {
        let params = QueryParams {
            argv: args.argv,
            collection: args.collection,
            options: args.options,
        };
        let access = gents_store::config_client::ConfigAccess::Graphql(self.caller(&ctx)?);
        let value=gents_tools::builtin::defra_query::execute_command(&access,&params,&self.options.scope).await
            .map_err(|error|ErrorData::internal_error(serde_json::json!({"error":format!("{error:#}"),"recovery":{"tool":"query","args":{"argv":["help"]}}}).to_string(),None))?;
        gents_tools::builtin::defra_query::render_result(value)
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
        let endpoint = self.caller(&ctx)?;
        let tool = gents_toolcalls::plugin::application_write::WriteTool::new(
            gents_store::config_client::ConfigAccess::Graphql(endpoint),
            self.options.write_collections.clone(),
            None,
        );
        let call = gents_toolcalls::plugin::application_write::WriteParams {
            argv: args.argv,
            collection: args.collection,
            options: args.options,
        };
        gents_loop::tool::Tool::call(&tool, call, &gents_loop::tool::ToolCallEnvelope::default())
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
        info.instructions = Some(String::from("Document access through query, and through write when explicitly exposed; call argv:[help] for commands. Every call requires caller-signed DefraDB bearer authorization and acts as that caller."));
        info
    }
}

pub fn defra_query_mcp_service(
    graphql: String,
    options: McpServiceOptions,
) -> StreamableHttpService<DefraQueryMcp, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(DefraQueryMcp::new(graphql.clone(), options.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAPHQL: &str = "http://127.0.0.1:1/api/v0/graphql";

    #[test]
    fn write_is_routed_only_with_write_collections() {
        let read_only = DefraQueryMcp::new(
            GRAPHQL.to_owned(),
            McpServiceOptions {
                scope: CollectionScope::all(),
                write_collections: BTreeSet::new(),
            },
        );
        assert!(read_only.tool_router.has_route("query"));
        assert!(!read_only.tool_router.has_route("write"));
    }
}

#[cfg(test)]
mod conformance {
    use super::caller_endpoint;

    #[tokio::test]
    async fn mcp_query_reader_matches_lean() {
        use gents_lean_vocab::families::runtime_reconcile_http_access_mcp_query_reads_as as family;
        for case in family::cases::<String>() {
            let identity = &case.input.identity;
            let presented = (identity != "anonymous").then(|| format!("Bearer {identity}"));
            let forwarded =
                match caller_endpoint("http://127.0.0.1:1/api/v0/graphql", presented.as_deref()) {
                    Some(endpoint) => endpoint.unwrap().runtime_authorization().await.unwrap(),
                    None => None,
                };
            case.check(
                forwarded.and_then(|header| header.strip_prefix("Bearer ").map(str::to_owned)),
            );
        }
    }
}
