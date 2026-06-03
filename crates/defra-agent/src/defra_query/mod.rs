//! Read-only structured query tool over DefraDB collections.
//!
//! `defra_query` lets an agent (or, in future, an external management surface)
//! read documents from DefraDB collections through a structured
//! `{collection, filter, fields, limit}` contract instead of hand-rolling
//! GraphQL. It is strictly read-only and renders all interpolated content
//! through [`crate::graphql::escape_graphql_string`].
//!
//! The query core ([`query::execute_query`]) is intentionally decoupled from
//! the [`rig::tool::Tool`] integration so the same logic can later back an
//! external (e.g. MCP/HTTP) management surface.

use std::sync::Arc;

use anyhow::anyhow;
use defra_node::EmbeddedNode;
use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolDyn};
use serde_json::json;

pub(crate) mod query;
pub(crate) mod render;

pub use query::{build_query, CollectionScope, DefraQueryParams, DEFAULT_LIMIT, MAX_LIMIT};

/// The model-facing tool name.
pub const DEFRA_QUERY_TOOL_NAME: &str = "defra_query";

/// Error wrapper mirroring the meta-tool convention: render the full anyhow
/// chain to the model.
#[derive(Debug)]
pub struct DefraQueryError(anyhow::Error);

impl std::fmt::Display for DefraQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for DefraQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for DefraQueryError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

/// Read-only structured query tool.
#[derive(Clone)]
pub struct DefraQueryTool {
    node: Arc<EmbeddedNode>,
    scope: CollectionScope,
}

impl DefraQueryTool {
    pub fn new(node: Arc<EmbeddedNode>, scope: CollectionScope) -> Self {
        Self { node, scope }
    }
}

impl Tool for DefraQueryTool {
    const NAME: &'static str = DEFRA_QUERY_TOOL_NAME;

    type Error = DefraQueryError;
    type Args = DefraQueryParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let scope_note = if self.scope.is_unrestricted() {
            "Any collection may be queried.".to_string()
        } else {
            "Only a restricted set of collections may be queried; an error lists them if you pick one outside the set.".to_string()
        };
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "Read documents from a DefraDB collection with a structured, read-only query. \
                 Provide a collection name, the fields to return, an optional DefraDB filter \
                 object, and an optional limit. Returns JSON: {{collection, count, results}}. \
                 Use this to inspect agent state and traces (e.g. AgentRequest, AgentResponse, \
                 AgentMessage, AgentToolCall, AgentSession) instead of hand-writing GraphQL. \
                 {scope_note}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "collection": {
                        "type": "string",
                        "description": "Collection (GraphQL type) name to read, e.g. \"AgentRequest\"."
                    },
                    "fields": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Field names to return, e.g. [\"request_id\", \"status\"]. Required, non-empty."
                    },
                    "filter": {
                        "type": "object",
                        "description": "Optional DefraDB filter object, e.g. {\"status\": {\"_eq\": \"pending\"}}. Supports operators (_eq, _ne, _gt, _lt, _ge, _le, _in, _nin, _like) and composition (_and, _or, _not)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum rows to return (default 50, capped at 1000)."
                    }
                },
                "required": ["collection", "fields"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let rows = query::execute_query(&self.node, &args, &self.scope).await?;
        let count = rows.as_array().map(|a| a.len()).unwrap_or(0);
        let payload = json!({
            "collection": args.collection,
            "count": count,
            "results": rows,
        });
        serde_json::to_string_pretty(&payload)
            .map_err(|e| DefraQueryError(anyhow!("failed to serialize query results: {e}")))
    }
}

/// Build the `defra_query` tool as a boxed `ToolDyn` for the tool surface.
pub fn build_defra_query_tool(node: Arc<EmbeddedNode>, scope: CollectionScope) -> Box<dyn ToolDyn> {
    Box::new(DefraQueryTool::new(node, scope))
}

#[cfg(test)]
mod tests;
