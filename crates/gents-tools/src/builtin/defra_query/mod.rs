use gents_protocol::tool_names::DEFRA_QUERY_TOOL_NAME;
use std::sync::Arc;

use anyhow::anyhow;
use defra_node::Node as EmbeddedNode;
use gents_loop::tool::ToolDefinition;
use gents_loop::tool::{Tool, ToolDyn};
use serde_json::json;

/// Bounds an individual field value included in model-facing query results.
pub(crate) const MAX_FIELD_STRING_BYTES: usize = 2_000;

fn field_truncation_marker(shown_bytes: usize, original_bytes: usize) -> String {
    format!(
        " [truncated: showed {} of {} bytes]",
        shown_bytes, original_bytes
    )
}

pub(crate) fn truncate_field_strings(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => {
            let original_bytes = s.len();
            if original_bytes > MAX_FIELD_STRING_BYTES {
                let truncated: String = s
                    .chars()
                    .scan(0usize, |acc, c| {
                        *acc += c.len_utf8();
                        if *acc <= MAX_FIELD_STRING_BYTES {
                            Some(c)
                        } else {
                            None
                        }
                    })
                    .collect();
                let marker = field_truncation_marker(truncated.len(), original_bytes);
                *s = format!("{}{}", truncated, marker);
                true
            } else {
                false
            }
        }
        serde_json::Value::Array(arr) => arr
            .iter_mut()
            .fold(false, |any, item| truncate_field_strings(item) | any),
        serde_json::Value::Object(map) => map
            .values_mut()
            .fold(false, |any, v| truncate_field_strings(v) | any),
        _ => false,
    }
}

pub mod bounded;
mod command;
#[cfg(test)]
pub(crate) use command::QueryCommandError;
mod field_page;
pub(crate) use field_page::{utf8_page, Utf8PageError};
mod native_filter;
mod relation;
mod search;
mod truncation_sidecar;
pub use command::{build_paged_query, execute_command, query_help, render_result, QueryParams};
pub use native_filter::validate_filter;
pub(crate) mod query;
pub mod render;
pub(crate) mod schema;

pub use bounded::BoundedQueryTool;
pub use query::{
    build_query, expand_collection_scope_aliases, is_restricted_field, CollectionScope,
    DefraQueryParams, AGENT_CONFIG_QUERY_COLLECTIONS, AGENT_CONFIG_SCOPE_ALIAS, DEFAULT_LIMIT,
    MAX_LIMIT,
};
pub use render::render_filter;
pub use schema::{diagnose_failed_query, unknown_collection_message};

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

#[derive(Clone)]
pub struct DefraQueryTool {
    node: Arc<EmbeddedNode>,
    scope: CollectionScope,
    actor: Option<::identity::Did>,
    /// Whether the calling agent holds the `tools` category of `config`,
    /// which decides who a refused write verb is told can grant writes.
    holds_tools: bool,
}

impl DefraQueryTool {
    pub fn new(node: Arc<EmbeddedNode>, scope: CollectionScope) -> Self {
        Self {
            node,
            scope,
            actor: None,
            holds_tools: false,
        }
    }
    pub fn with_tools_category(mut self, held: bool) -> Self {
        self.holds_tools = held;
        self
    }
    pub fn with_actor(mut self, actor: ::identity::Did) -> Self {
        self.actor = Some(actor);
        self
    }
}

impl Tool for DefraQueryTool {
    const NAME: &'static str = DEFRA_QUERY_TOOL_NAME;
    type Error = DefraQueryError;
    type Args = QueryParams;
    type Output = String;
    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.into(),
            description: "Read application and runtime documents. Pass only the verb in argv ([\"fields\"], [\"find\"], [\"count\"], [\"search\"], [\"explain\"], or [\"help\",COMMAND]) and put the collection name in collection (never in argv). Count aggregates every matching row; find returns a bounded ordered page; search ranks by BM25 (options.text + options.search_fields) or vector (options.vector + options.vector_field). Use argv:[\"explain\"] (not an option on find) to inspect a native plan without running it (options.mode:\"simple\", default) or to measure a bounded read (options.mode:\"execute\", only when requested); explain is read-only. Configuration uses config, definitions use schema.".into(),
            parameters: json!({
                "type": "object",
                "required": ["argv"],
                "additionalProperties": false,
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "Verb only: [\"fields\"], [\"find\"], [\"count\"], [\"search\"], [\"explain\"], or [\"help\"] / [\"help\",COMMAND]. Never put the collection name in argv; pass it in collection."
                    },
                    "collection": {
                        "type": "string",
                        "description": "GraphQL collection name (required for fields, find, count, search, explain; omit for help). Discover with schema collection list. Surface IDs belong to config datastore get."
                    },
                    "options": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Command options. find/search/explain require fields; count accepts only filter; fields takes no options.",
                        "properties": {
                            "fields": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Required for find, search, explain: field names to return (e.g. [\"name\", \"relation.field\"]). Discover with argv:[\"fields\"]."
                            },
                            "filter": {
                                "type": "object",
                                "description": "Native DefraDB filter object using _eq, _neq, _gt, _geq, _lt, _leq, _in, _nin, _like, _ilike, _and, _or, _not."
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Max rows to return (find default 50, search default 10, max 1000)."
                            },
                            "offset": {
                                "type": "integer",
                                "description": "Row offset for find/explain (0–100000)."
                            },
                            "order": {
                                "type": "array",
                                "items": {"type": "object"},
                                "description": "Sort order for find/search/explain, e.g. [{\"priority\":\"ASC\"}] (ASC or DESC)."
                            },
                            "text": {
                                "type": "string",
                                "description": "For search (BM25): nonempty keyword query string (pair with search_fields; not \"query\" or \"keywords\")."
                            },
                            "search_fields": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "For search (BM25): String field names to score against options.text."
                            },
                            "vector_field": {
                                "type": "string",
                                "description": "For search (vector): numeric list field name (pair with vector)."
                            },
                            "vector": {
                                "type": "array",
                                "items": {"type": "number"},
                                "description": "For search (vector): query vector numbers matching vector_field dimensions."
                            },
                            "mode": {
                                "type": "string",
                                "description": "For argv:[\"explain\"] only (not find): \"simple\" (default, inspect plan without running) or \"execute\" (run bounded read for native execution metrics)."
                            },
                            "field_page": {
                                "type": "object",
                                "description": "For find: read a truncated string page ({doc_id, field, offset_bytes, expected_hash})."
                            },
                            "signer": {
                                "type": "boolean",
                                "description": "For find: set true to include _signer provenance."
                            }
                        }
                    }
                }
            }),
        }
    }
    async fn call(
        &self,
        args: Self::Args,
        _context: &gents_loop::tool::ToolCallEnvelope,
    ) -> Result<String, Self::Error> {
        if let Some(verb) = command::write_verb(&args) {
            return Err(anyhow!("{}", json!({
                "error": format!("query only reads; it has no {verb} command. Writes go through the write tool"),
                "recovery": crate::builtin::defra_write::write_grant_guidance(self.holds_tools, None, args.collection.as_slice()),
            })).into());
        }
        let result = if let Some(actor) = &self.actor {
            gents_store::config_client::ConfigAccess::transact_local_readonly(
                &self.node,
                Some(actor.clone()),
                "application_query",
                |txn| {
                    Box::pin(command::execute_typed_command(
                        txn,
                        &args,
                        &self.scope,
                        None,
                    ))
                },
            )
            .await
        } else {
            command::execute_typed_command(
                &gents_store::config_client::ConfigAccess::local(self.node.clone()),
                &args,
                &self.scope,
                None,
            )
            .await
        };
        match result {
            Ok(value) => render_result(value).map_err(Into::into),
            Err(error @ command::QueryCommandError::Discovery(_)) => Err(anyhow!("{}", json!({"error":format!("{error:#}"),"recovery":{"tool":"schema","args":{"argv":["collection","list"]}}})).into()),
            Err(command::QueryCommandError::Refused(refusal)) => Err(anyhow!("{}", json!({"error":refusal.message,"recovery":refusal.next_call})).into()),
            Err(error) => {
                let error = error.erased();
                Err(anyhow!("{}", json!({"error":format!("{error:#}"),"recovery":{"tool":"query","args":{"argv":["help",args.argv.first().filter(|command| query_help(Some(command)).is_ok()).map(String::as_str).unwrap_or("find")]}}})).into())
            }
        }
    }
}

pub fn build_defra_query_tool(
    node: Arc<EmbeddedNode>,
    scope: CollectionScope,
    actor: ::identity::Did,
    holds_tools: bool,
) -> Box<dyn ToolDyn> {
    Box::new(
        DefraQueryTool::new(node, scope)
            .with_actor(actor)
            .with_tools_category(holds_tools),
    )
}

#[cfg(test)]
mod tests;

pub use query::collect_filter_field_keys;
