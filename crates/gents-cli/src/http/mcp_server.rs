use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::FromRequestParts as _;
use gents::defra_query::{CollectionScope, QueryParams};
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, JsonObject, Tool};
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::RoleServer;
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

/// What `/mcp` exposes; present only with `--enable-mcp`.
#[derive(Clone)]
pub(crate) struct McpServiceOptions {
    pub scope: CollectionScope,
    pub write_collections: BTreeSet<String>,
    /// `--mcp-graph-tools`: the read-only graph tools, each read made under
    /// the caller's forwarded DefraDB bearer. Set only with an unrestricted
    /// `scope` (the CLI refuses the flag with `--mcp-query-collection`): the
    /// graph collections declare no per-caller policy, and the run views also
    /// read request, callback and trigger documents.
    pub graph_reads: bool,
}

#[derive(Clone)]
pub(crate) struct DefraQueryMcp {
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
        if options.graph_reads {
            for definition in gents::self_config::graph_tool_definitions() {
                if gents::self_config::MCP_GRAPH_READ_TOOL_NAMES.contains(&definition.name.as_str())
                {
                    tool_router.add_route(graph_read_route(definition));
                }
            }
        }
        Self {
            graphql,
            options,
            tool_router,
        }
    }

    /// One graph read for the caller this request authenticates, through the
    /// owners and reply shapes of the in-session tools. The read carries the
    /// caller's forwarded bearer, which DefraDB authenticates; the graph
    /// collections declare no `@policy`, so DefraDB admits the read, and the
    /// caller's DID only selects the subject (the listing's owner, the run
    /// views' observer). A result document is rendered only when the MCP read
    /// scope admits its collection. The listing is the in-session reply text,
    /// returned as is so that no re-serialization reorders it.
    async fn call_graph_read(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<String, ErrorData> {
        let parts = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .ok_or_else(|| ErrorData::invalid_request(BEARER_REQUIRED, None))?;
        let (access, did) = caller_graph_access(&self.graphql, parts).await?;
        let arguments = Value::Object(arguments.unwrap_or_default());
        let invalid =
            |error: serde_json::Error| ErrorData::invalid_params(format!("{name}: {error}"), None);
        let read: anyhow::Result<Value> = match name {
            gents::self_config::LIST_GRAPHS_TOOL_NAME => {
                serde_json::from_value::<gents::self_config::ListGraphsParams>(arguments)
                    .map_err(invalid)?;
                return gents::self_config::list_graphs_value(&access, &did, None)
                    .await
                    .map_err(|error| ErrorData::internal_error(format!("{error:#}"), None));
            }
            gents::self_config::GET_GRAPH_RUN_TOOL_NAME => {
                let args =
                    serde_json::from_value::<gents::self_config::GraphRunIdParams>(arguments)
                        .map_err(invalid)?;
                gents::graph_pipeline::load_graph_run_view_with_access(&access, &did, &args.run_id)
                    .await
                    .and_then(|view| Ok(serde_json::to_value(view)?))
            }
            gents::self_config::GET_GRAPH_RESULT_TOOL_NAME => {
                let args =
                    serde_json::from_value::<gents::self_config::GraphRunIdParams>(arguments)
                        .map_err(invalid)?;
                gents::graph_pipeline::load_graph_run_result_view_with_access(
                    &access,
                    &did,
                    &args.run_id,
                )
                .await
                .and_then(|view| result_value_in_scope(&self.options.scope, view))
            }
            other => {
                return Err(ErrorData::internal_error(
                    format!("{other} is not a graph read"),
                    None,
                ))
            }
        };
        let value = read.map_err(|error| ErrorData::internal_error(format!("{error:#}"), None))?;
        serde_json::to_string_pretty(&value)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
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
        let value=gents::defra_query::execute_command(&access,&params,&self.options.scope).await
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
            self.options.write_collections.clone(),
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
        let mut instructions = String::from("Read-only document access through query; call argv:[help] for commands. Writes, when explicitly exposed, require caller-signed DefraDB bearer authorization.");
        if self.options.graph_reads {
            instructions.push_str(" Graph reads (list_graphs, get_graph_run, get_graph_result) need a caller-signed DefraDB bearer minted for this host:port and read as that caller.");
        }
        info.instructions = Some(instructions);
        info
    }
}

const BEARER_REQUIRED: &str = "graph reads require a caller-signed DefraDB Bearer authorization minted for this request's host:port; query remains anonymous";
const BEARER_SCHEME_REQUIRED: &str =
    "graph reads forward the DefraDB bearer only with the `Bearer ` scheme (capital B)";

/// An MCP route for one in-session graph read: the same name, description
/// and parameter schema, answered by [`DefraQueryMcp::call_graph_read`].
fn graph_read_route(definition: gents::llm::tool::ToolDefinition) -> ToolRoute<DefraQueryMcp> {
    let schema = definition
        .parameters
        .as_object()
        .cloned()
        .unwrap_or_default();
    let name = definition.name.clone();
    ToolRoute::new_dyn(
        Tool::new(definition.name, definition.description, Arc::new(schema)),
        move |context: ToolCallContext<'_, DefraQueryMcp>| {
            let name = name.clone();
            Box::pin(async move {
                let text = context
                    .service
                    .call_graph_read(&name, context.arguments, &context.request_context)
                    .await?;
                Ok(CallToolResult::success(vec![Content::text(text)]))
            })
        },
    )
}

/// The caller's own access for a graph read. Its DefraDB bearer is forwarded
/// unchanged, exactly as `write` forwards it, so DefraDB authenticates the
/// read; the graph collections declare no `@policy`, so DefraDB admits it.
/// The DID is the one DefraDB's own extractor verifies in that bearer, which
/// the read owners take as their subject (the owner filter of the listing,
/// the observer of a run view); it selects what is shown and is not an access
/// boundary. `/mcp` is merged
/// after DefraDB's auth middleware, so the middleware does not run here; the
/// extractor checks signature, expiry and the request `Host` as audience, and
/// records the bearer for DefraDB's ACP passthrough as it does for every
/// DefraDB request. An anonymous caller is refused before any read. The
/// Authorization header is never logged or echoed.
async fn caller_graph_access(
    graphql: &str,
    parts: &axum::http::request::Parts,
) -> Result<(gents::config_client::ConfigAccess, String), ErrorData> {
    let authorization = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .ok_or_else(|| ErrorData::invalid_request(BEARER_REQUIRED, None))?;
    let did = defra_http::ExtractIdentity::from_request_parts(&mut parts.clone(), &())
        .await
        .map_err(|rejection| ErrorData::invalid_request(bearer_refusal(&rejection), None))?
        .into_did()
        .ok_or_else(|| ErrorData::invalid_request(BEARER_REQUIRED, None))?;
    let endpoint = gents::config_client::GraphqlEndpoint::with_delegated_authorization(
        graphql.to_owned(),
        authorization,
    )
    .map_err(|_| ErrorData::invalid_request(BEARER_SCHEME_REQUIRED, None))?;
    Ok((
        gents::config_client::ConfigAccess::Graphql(endpoint),
        did.as_str().to_owned(),
    ))
}

/// A result document is rendered only when the MCP read scope admits its
/// collection; `CollectionScope::ensure_allowed` also refuses the protected
/// eval and optimization collections under an unrestricted scope.
fn result_value_in_scope(
    scope: &CollectionScope,
    view: gents::graph_pipeline::GraphRunView,
) -> anyhow::Result<Value> {
    for reference in view
        .results
        .iter()
        .flat_map(|result| &result.refs)
        .chain(&view.persisted_result_refs)
    {
        scope.ensure_allowed(&reference.collection)?;
    }
    Ok(serde_json::to_value(view)?)
}

fn bearer_refusal(rejection: &defra_http::IdentityExtractionError) -> &'static str {
    match rejection {
        defra_http::IdentityExtractionError::InvalidToken(_) => {
            "the Authorization header is not a valid DefraDB bearer"
        }
        defra_http::IdentityExtractionError::TokenVerificationFailed(_) => {
            "the DefraDB bearer is expired or was minted for another host:port; mint one for this request's Host"
        }
        defra_http::IdentityExtractionError::MissingHost(_) => {
            "a DefraDB bearer needs the request's Host header"
        }
        _ => "the DefraDB bearer was refused; mint one for this request's Host",
    }
}

pub(crate) fn defra_query_mcp_service(
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
    use gents::AgentIdentity as _;

    const GRAPHQL: &str = "http://127.0.0.1:1/api/v0/graphql";

    fn options(graph_reads: bool) -> McpServiceOptions {
        McpServiceOptions {
            scope: CollectionScope::all(),
            write_collections: BTreeSet::new(),
            graph_reads,
        }
    }

    fn request_parts(headers: &[(axum::http::HeaderName, &str)]) -> axum::http::request::Parts {
        let mut request = axum::http::Request::builder().uri("/mcp");
        for (name, value) in headers {
            request = request.header(name, *value);
        }
        request.body(()).unwrap().into_parts().0
    }

    #[test]
    fn graph_read_routes_are_the_in_session_definitions() {
        let on = DefraQueryMcp::new(GRAPHQL.to_owned(), options(true));
        let mut offered = Vec::new();
        for definition in gents::self_config::graph_tool_definitions() {
            if let Some(route) = on.tool_router.get(&definition.name) {
                assert_eq!(
                    route.description.as_deref(),
                    Some(definition.description.as_str())
                );
                assert_eq!(
                    Value::Object((*route.input_schema).clone()),
                    definition.parameters
                );
                offered.push(definition.name);
            }
        }
        assert_eq!(offered, gents::self_config::MCP_GRAPH_READ_TOOL_NAMES);
    }

    #[test]
    fn graph_read_routes_are_absent_unless_enabled() {
        let off = DefraQueryMcp::new(GRAPHQL.to_owned(), options(false));
        for definition in gents::self_config::graph_tool_definitions() {
            assert!(
                !off.tool_router.has_route(&definition.name),
                "{}",
                definition.name
            );
        }
        assert!(off.tool_router.has_route("query") && !off.tool_router.has_route("write"));
    }

    #[tokio::test]
    async fn caller_graph_access_forwards_the_callers_bearer_unchanged() {
        let keys = tempfile::tempdir().unwrap();
        let caller =
            gents::KeyIdentity::load_or_create(keys.path().join("caller.key"), None).unwrap();
        let bearer =
            gents::identity::defradb_bearer_authorization(caller.did(), "127.0.0.1:1").unwrap();
        let parts = request_parts(&[
            (axum::http::header::HOST, "127.0.0.1:1"),
            (axum::http::header::AUTHORIZATION, bearer.as_str()),
        ]);
        let (access, did) = caller_graph_access(GRAPHQL, &parts).await.unwrap();
        assert_eq!(did, caller.did());
        let gents::config_client::ConfigAccess::Graphql(endpoint) = access else {
            panic!("graph reads go to DefraDB over HTTP");
        };
        assert_eq!(endpoint.url(), GRAPHQL);
        assert_eq!(
            endpoint.authorization().unwrap().as_deref(),
            Some(bearer.as_str()),
            "the caller's bearer is forwarded unchanged and never re-minted"
        );
    }

    #[tokio::test]
    async fn caller_graph_access_names_the_scheme_it_forwards() {
        let keys = tempfile::tempdir().unwrap();
        let caller =
            gents::KeyIdentity::load_or_create(keys.path().join("caller.key"), None).unwrap();
        let bearer =
            gents::identity::defradb_bearer_authorization(caller.did(), "127.0.0.1:1").unwrap();
        let lowercase = bearer.replacen("Bearer ", "bearer ", 1);
        let parts = request_parts(&[
            (axum::http::header::HOST, "127.0.0.1:1"),
            (axum::http::header::AUTHORIZATION, lowercase.as_str()),
        ]);
        let refused = caller_graph_access(GRAPHQL, &parts)
            .await
            .err()
            .expect("only the `Bearer` scheme is forwarded");
        assert_eq!(refused.message, BEARER_SCHEME_REQUIRED);
    }

    #[tokio::test]
    async fn caller_graph_access_refuses_an_anonymous_request() {
        for headers in [
            vec![(axum::http::header::HOST, "127.0.0.1:1")],
            vec![
                (axum::http::header::HOST, "127.0.0.1:1"),
                (axum::http::header::AUTHORIZATION, "Bearer "),
            ],
        ] {
            let refused = caller_graph_access(GRAPHQL, &request_parts(&headers))
                .await
                .err()
                .expect("an anonymous request is refused before any read");
            assert!(
                refused.message.contains("caller-signed DefraDB Bearer"),
                "{}",
                refused.message
            );
        }
    }

    fn result_view(result_collection: &str, persisted_collection: &str) -> Value {
        let reference = |collection: &str| {
            serde_json::json!({
                "name": "findings", "collection": collection,
                "document_id": "bae-1", "commit_cid": "cid-1",
            })
        };
        serde_json::json!({
            "view_version": 1, "run_id": "run-1", "graph_id": "graph",
            "revision_digest": "sha256:d", "owner_did": "did:test:owner",
            "caller_did": "did:test:owner", "entry_name": "entry", "correlation": "corr",
            "status": "succeeded", "input": {}, "created_at": "2026-10-02T05:30:00Z",
            "update_generation": 1, "requests": [], "stages": [], "groups": [],
            "results": [{
                "name": "findings", "terminal": true, "satisfied": true,
                "observed_count": 1, "violation": null,
                "refs": [reference(result_collection)], "documents": [],
            }],
            "persisted_result_refs": [reference(persisted_collection)],
            "active_request_count": 0, "terminal_request_count": 0,
            "result_contract_satisfied": true, "failure_evidence": null,
        })
    }

    #[test]
    fn graph_result_refuses_a_protected_result_collection() {
        let all = CollectionScope::all();
        let admitted = serde_json::from_value(result_view("Finding", "Finding")).unwrap();
        assert!(result_value_in_scope(&all, admitted).is_ok());
        for view in [
            result_view("EvalRun", "Finding"),
            result_view("Finding", "EvalRun"),
        ] {
            let refused =
                result_value_in_scope(&all, serde_json::from_value(view).unwrap()).unwrap_err();
            assert!(
                format!("{refused:#}").contains("\"EvalRun\" is protected"),
                "{refused:#}"
            );
        }
    }
}
