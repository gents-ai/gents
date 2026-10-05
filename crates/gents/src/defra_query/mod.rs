//! Generic document reads share scope, credential and GraphQL rendering owners.

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
            .fold(false, |any, item| truncate_field_strings(item) || any),
        serde_json::Value::Object(map) => map
            .values_mut()
            .fold(false, |any, v| truncate_field_strings(v) || any),
        _ => false,
    }
}

pub(crate) mod bounded;
mod command;
mod field_page;
mod native_filter;
mod search;
pub use command::{build_paged_query, execute_command, query_help, render_result, QueryParams};
pub(crate) use native_filter::validate_filter;
pub(crate) mod query;
pub(crate) mod render;
pub(crate) mod schema;

pub use bounded::BoundedQueryTool;
pub use query::{
    build_query, expand_collection_scope_aliases, CollectionScope, DefraQueryParams,
    AGENT_CONFIG_QUERY_COLLECTIONS, AGENT_CONFIG_SCOPE_ALIAS, DEFAULT_LIMIT, MAX_LIMIT,
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
            crate::config_client::ConfigAccess::transact_local(
                &self.node,
                Some(actor.clone()),
                "application_query",
                |txn| Box::pin(execute_command(txn, &args, &self.scope)),
            )
            .await
        } else {
            execute_command(
                &crate::config_client::ConfigAccess::Local(self.node.clone()),
                &args,
                &self.scope,
            )
            .await
        };
        match result {
            Ok(value) => render_result(value).map_err(Into::into),
            Err(error) if error.is::<command::CollectionDiscoveryError>() => Err(anyhow!("{}", json!({"error":format!("{error:#}"),"recovery":{"tool":"schema","args":{"argv":["collection","list"]}}})).into()),
            Err(error) => Err(anyhow!("{}", json!({"error":format!("{error:#}"),"recovery":{"tool":"query","args":{"argv":["help",args.argv.first().filter(|command| query_help(Some(command)).is_ok()).map(String::as_str).unwrap_or("find")]}}})).into()),
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

#[cfg(test)]
mod truncation_tests {
    use serde_json::{json, Value};

    use super::{truncate_field_strings, MAX_FIELD_STRING_BYTES};

    /// A string field exceeding the cap is truncated and the marker is appended.
    #[test]
    fn oversized_string_field_is_truncated_with_honest_marker() {
        let big_string = "a".repeat(MAX_FIELD_STRING_BYTES + 1000);
        let mut value = json!({ "content": big_string });
        let truncated = truncate_field_strings(&mut value);

        assert!(truncated, "must report that truncation occurred");

        let result = value["content"]
            .as_str()
            .expect("field must remain a string");
        assert!(
            result.len() < big_string.len(),
            "truncated field must be shorter than original: got {}",
            result.len()
        );
        assert!(
            result.contains("[truncated: showed"),
            "honest marker must be present: {result}"
        );
        assert!(
            result.contains("bytes]"),
            "total byte count must appear in marker: {result}"
        );
    }

    /// A string field within the cap is left unchanged.
    #[test]
    fn small_string_field_passes_through_unchanged() {
        let small = "hello world".to_string();
        let mut value = json!({ "content": small });
        let truncated = truncate_field_strings(&mut value);

        assert!(!truncated);
        assert_eq!(value["content"].as_str().unwrap(), "hello world");
    }

    /// Non-string fields (numbers, booleans, nulls) are never modified.
    #[test]
    fn non_string_fields_are_never_modified() {
        let mut value = json!({
            "count": 42,
            "active": true,
            "score": 2.5,
            "missing": null
        });
        let truncated = truncate_field_strings(&mut value);

        assert!(!truncated);
        assert_eq!(value["count"], json!(42));
        assert_eq!(value["active"], json!(true));
    }

    /// Nested objects and arrays are recursively walked.
    #[test]
    fn nested_objects_and_arrays_are_recursively_truncated() {
        let big = "z".repeat(MAX_FIELD_STRING_BYTES + 500);
        let mut value = json!({
            "rows": [
                { "text": big.clone(), "id": 1 },
                { "text": "short", "id": 2 }
            ]
        });
        let truncated = truncate_field_strings(&mut value);

        assert!(truncated);
        let first_text = value["rows"][0]["text"].as_str().expect("string");
        assert!(
            first_text.contains("[truncated: showed"),
            "marker on big field"
        );
        assert_eq!(value["rows"][1]["text"].as_str().unwrap(), "short");
        assert_eq!(value["rows"][0]["id"], json!(1)); // number unchanged
    }

    /// After truncation the resulting JSON must still be valid/parseable.
    #[test]
    fn result_json_is_valid_after_truncation() {
        let big = "b".repeat(MAX_FIELD_STRING_BYTES * 3);
        let mut rows = json!([
            { "body": big, "status": "pending" },
            { "body": "small", "status": "done" }
        ]);
        truncate_field_strings(&mut rows);

        // Build the full envelope (same shape as the tool's call() output).
        let payload = json!({
            "collection": "AgentRequest",
            "count": 2,
            "truncated": true,
            "total_bytes": 999,
            "results": rows,
        });
        let serialized =
            serde_json::to_string_pretty(&payload).expect("must serialize without error");
        let reparsed: Value =
            serde_json::from_str(&serialized).expect("must be parseable after truncation");

        assert_eq!(reparsed["collection"], "AgentRequest");
        assert_eq!(reparsed["count"], 2);
        assert_eq!(reparsed["truncated"], true);
        let body = reparsed["results"][0]["body"].as_str().unwrap();
        assert!(body.contains("[truncated: showed"));
    }

    /// When no field exceeds the cap, `truncated` is false in the envelope.
    #[test]
    fn small_rows_produce_truncated_false_envelope() {
        let mut rows = json!([
            { "request_id": "req-1", "status": "pending" },
            { "request_id": "req-2", "status": "done" }
        ]);
        let truncated = truncate_field_strings(&mut rows);
        assert!(!truncated);
    }
}
