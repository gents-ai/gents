use gents_protocol::error_chain::ErrorChain;
use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{Tool, ToolDyn};
use anyhow::anyhow;
use defra_node::EmbeddedNode;
use serde_json::json;

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

pub(crate) mod bounded;
mod command;
mod error;
mod field_page;
pub(crate) use field_page::{utf8_page, Utf8PageError};
mod native_filter;
mod search;
pub use command::{build_paged_query, execute_command, query_help, render_result, QueryParams};
pub(crate) use native_filter::validate_filter;
pub(crate) mod query;
pub(crate) mod render;
pub(crate) mod schema;

pub use bounded::BoundedQueryTool;
pub use error::DefraQueryFailure;
pub use query::{
    build_query, expand_collection_scope_aliases, is_restricted_field, CollectionScope,
    DefraQueryParams, AGENT_CONFIG_QUERY_COLLECTIONS, AGENT_CONFIG_SCOPE_ALIAS, DEFAULT_LIMIT,
    MAX_LIMIT,
};
pub use schema::{
    diagnose_failed_query, discovery_payload, introspection_query, parse_collection_schema,
    unknown_collection_message, CollectionSchema, SchemaField,
};

pub const DEFRA_QUERY_TOOL_NAME: &str = "query";

#[derive(Debug)]
pub struct DefraQueryError(anyhow::Error);

impl std::fmt::Display for DefraQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", ErrorChain(self.0.as_ref()))
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

impl From<DefraQueryFailure> for DefraQueryError {
    fn from(error: DefraQueryFailure) -> Self {
        Self(error.into())
    }
}

#[derive(Clone)]
pub struct DefraQueryTool {
    node: Arc<EmbeddedNode>,
    scope: CollectionScope,
    actor: Option<::identity::Did>,
}

impl DefraQueryTool {
    pub fn new(node: Arc<EmbeddedNode>, scope: CollectionScope) -> Self {
        Self {
            node,
            scope,
            actor: None,
        }
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
        ToolDefinition {name:Self::NAME.into(),description:"Read application and runtime documents. argv: [fields], [find], [count], [search], [explain], or [help,COMMAND]. Count aggregates every matching row; find returns a bounded ordered page. Explain inspects a native plan; executing it requires options.mode:execute. Configuration uses config, definitions use schema.".into(),parameters:json!({"type":"object","required":["argv"],"additionalProperties":false,"properties":{"argv":{"type":"array","items":{"type":"string"}},"collection":{"type":"string","description":"GraphQL collection name. Discover with schema collection list. Surface IDs belong to config datastore get."},"options":{"type":"object"}}})}
    }
    async fn call(&self, args: Self::Args) -> Result<String, Self::Error> {
        let result = if let Some(actor) = &self.actor {
            crate::config_client::ConfigAccess::transact_local_readonly(
                &self.node,
                Some(actor.clone()),
                "application_query",
                |txn| Box::pin(execute_command(txn, &args, &self.scope)),
            )
            .await
        } else {
            execute_command(
                &crate::config_client::ConfigAccess::local(self.node.clone()),
                &args,
                &self.scope,
            )
            .await
        };
        match result {
            Ok(value) => render_result(value).map_err(Into::into),
            Err(error) if error.is::<command::CollectionDiscoveryError>() => Err(anyhow!("{}", json!({"error":format!("{}", ErrorChain(error.as_ref())),"recovery":{"tool":"schema","args":{"argv":["collection","list"]}}})).into()),
            Err(error) => Err(anyhow!("{}", json!({"error":format!("{}", ErrorChain(error.as_ref())),"recovery":{"tool":"query","args":{"argv":["help",args.argv.first().filter(|command| query_help(Some(command)).is_ok()).map(String::as_str).unwrap_or("find")]}}})).into()),
        }
    }
}

pub fn build_defra_query_tool(
    node: Arc<EmbeddedNode>,
    scope: CollectionScope,
    actor: ::identity::Did,
) -> Box<dyn ToolDyn> {
    Box::new(DefraQueryTool::new(node, scope).with_actor(actor))
}

#[cfg(test)]
mod tests;
