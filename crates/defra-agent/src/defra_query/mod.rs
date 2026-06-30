//! Read-only structured query tool over DefraDB collections.
//!
//! `defra_query` lets an agent (or, in future, an external management surface)
//! read documents from DefraDB collections through a structured
//! `{collection, filter, fields, limit}` contract instead of hand-rolling
//! GraphQL. It is strictly read-only and renders all interpolated content
//! through [`crate::graphql::escape_graphql_string`].
//!
//! The query core ([`query::execute_query`]) is intentionally decoupled from
//! the [`crate::llm::tool::Tool`] integration so the same logic can later back an
//! external (e.g. MCP/HTTP) management surface.

use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{Tool, ToolDyn};
use anyhow::anyhow;
use defra_node::EmbeddedNode;
use serde_json::json;

/// Maximum number of UTF-8 bytes kept per string field value in the query
/// results. Longer values are replaced with a truncated copy + honest marker.
/// This preserves JSON structure (the result stays parseable) while bounding
/// the per-turn byte budget for large text or blob fields.
pub(crate) const MAX_FIELD_STRING_BYTES: usize = 2_000;

/// Marker appended to truncated string field values. Intentionally matches the
/// style used by `crate::truncation::logic::truncate_text` (honest + totals).
fn field_truncation_marker(original_bytes: usize) -> String {
    format!(
        " [truncated: showed {} of {} bytes]",
        MAX_FIELD_STRING_BYTES, original_bytes
    )
}

/// Walk a JSON `Value` (typically the array of result rows) and replace every
/// string value that exceeds [`MAX_FIELD_STRING_BYTES`] with a truncated copy
/// that ends with an honest marker. Returns `true` when at least one field was
/// truncated, so the caller can surface `truncated: true` in the envelope.
///
/// Non-string values (numbers, booleans, nulls, nested objects/arrays) are
/// passed through unchanged — JSON structure is never broken.
pub(crate) fn truncate_field_strings(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => {
            let original_bytes = s.len();
            if original_bytes > MAX_FIELD_STRING_BYTES {
                // Truncate at a char boundary so the JSON string stays valid UTF-8.
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
                let marker = field_truncation_marker(original_bytes);
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
        let mut rows = query::execute_query(&self.node, &args, &self.scope).await?;
        let count = rows.as_array().map(|a| a.len()).unwrap_or(0);

        // Measure total bytes before any truncation so the model can see the
        // unabridged size even when we trim individual fields.
        let total_bytes = serde_json::to_string(&rows).map(|s| s.len()).unwrap_or(0);

        // Per-field string truncation: keeps the JSON structure valid/parseable
        // while bounding large text/blob fields that could blow the context window.
        let truncated = truncate_field_strings(&mut rows);

        let mut payload = json!({
            "collection": args.collection,
            "count": count,
            "truncated": truncated,
            "total_bytes": total_bytes,
            "results": rows,
        });

        // When fields were truncated, add a note so the model understands what happened.
        if truncated {
            payload["truncation_note"] = json!(format!(
                "One or more string fields were truncated to {} bytes; total untruncated result size was {} bytes. \
                 Use more specific filters or fewer fields to retrieve full values.",
                MAX_FIELD_STRING_BYTES, total_bytes
            ));
        }

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
