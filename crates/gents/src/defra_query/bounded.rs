//! Declarative, schema-bounded single-collection query tool.
//!
//! Sibling of [`crate::defra_write::BoundedWriteTool`]: each instance is locked
//! to one [`QueryToolDecl`] — one collection, a fixed projection, optional
//! runtime-filled filters. The model never names the collection.

use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use defra_node::EmbeddedNode;
use serde_json::{json, Map, Value};

use crate::document_config::{QueryToolDecl, WriteToolFieldFill};
use crate::llm::tool::{Tool, ToolDefinition};

use super::query::{self, CollectionScope, DefraQueryParams, MAX_LIMIT};
use super::{truncate_field_strings, MAX_FIELD_STRING_BYTES};

const PLACEHOLDER_TOOL_NAME: &str = "defra_query_bound";

#[derive(Debug)]
pub struct DefraBoundQueryError(anyhow::Error);

impl std::fmt::Display for DefraBoundQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for DefraBoundQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for DefraBoundQueryError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(transparent)]
pub struct BoundedQueryParams(pub Map<String, Value>);

#[derive(Clone)]
pub struct BoundedQueryTool {
    node: Arc<EmbeddedNode>,
    decl: QueryToolDecl,
}

impl BoundedQueryTool {
    pub fn new(node: Arc<EmbeddedNode>, decl: QueryToolDecl) -> Self {
        Self { node, decl }
    }

    pub fn is_well_formed(&self) -> bool {
        self.decl.is_well_formed()
    }

    fn model_filter_fields(&self) -> impl Iterator<Item = &crate::document_config::WriteToolField> {
        self.decl
            .filter_fields
            .iter()
            .filter(|field| field.fill.is_none())
    }

    fn filled_filter_fields(
        &self,
    ) -> impl Iterator<Item = &crate::document_config::WriteToolField> {
        self.decl
            .filter_fields
            .iter()
            .filter(|field| field.fill.is_some())
    }

    fn resolve_projection(&self, args: &Map<String, Value>) -> Result<Vec<String>> {
        match args.get("fields") {
            None | Some(Value::Null) => Ok(self.decl.fields.clone()),
            Some(Value::Array(items)) => {
                let mut fields = Vec::new();
                for item in items {
                    let Some(name) = item.as_str().map(str::trim).filter(|name| !name.is_empty())
                    else {
                        bail!(
                            "tool `{}` fields must be a list of field names",
                            self.decl.tool_name
                        );
                    };
                    if !self.decl.fields.iter().any(|allowed| allowed == name) {
                        bail!(
                            "field `{name}` is not in the projection allowlist for tool `{}`",
                            self.decl.tool_name
                        );
                    }
                    if query::is_restricted_field(&self.decl.collection, name) {
                        bail!(
                            "field `{name}` on {:?} is restricted and cannot be queried",
                            self.decl.collection
                        );
                    }
                    if !fields.iter().any(|existing| existing == name) {
                        fields.push(name.to_string());
                    }
                }
                if fields.is_empty() {
                    bail!(
                        "tool `{}` fields must list at least one allowed field",
                        self.decl.tool_name
                    );
                }
                Ok(fields)
            }
            Some(_) => bail!(
                "tool `{}` fields must be a list of field names",
                self.decl.tool_name
            ),
        }
    }

    fn resolve_limit(&self, args: &Map<String, Value>) -> Result<u32> {
        match args.get("limit") {
            None | Some(Value::Null) => Ok(MAX_LIMIT),
            Some(Value::Number(number)) => {
                let Some(limit) = number.as_u64().and_then(|value| u32::try_from(value).ok())
                else {
                    bail!(
                        "tool `{}` limit must be a positive integer",
                        self.decl.tool_name
                    );
                };
                Ok(limit.clamp(1, MAX_LIMIT))
            }
            Some(_) => bail!(
                "tool `{}` limit must be a positive integer",
                self.decl.tool_name
            ),
        }
    }

    fn resolve_filter(&self, args: &Map<String, Value>) -> Result<Option<Value>> {
        for key in args.keys() {
            if key == "fields" || key == "limit" {
                continue;
            }
            if self.filled_filter_fields().any(|field| field.name == *key) {
                bail!(
                    "filter `{key}` is runtime-filled and must not be supplied to tool `{}`",
                    self.decl.tool_name
                );
            }
            if !self.model_filter_fields().any(|field| field.name == *key) {
                bail!(
                    "filter `{key}` is not permitted by tool `{}`",
                    self.decl.tool_name
                );
            }
        }

        let mut filter = Map::new();
        for field in &self.decl.filter_fields {
            let value = if let Some(fill) = &field.fill {
                let runtime = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
                    .ok_or_else(|| {
                    anyhow!(
                        "runtime-filled filter `{}` requires an AgentRequest trigger context",
                        field.name
                    )
                })?;
                let filled = match fill {
                    WriteToolFieldFill::Correlation => runtime
                        .correlation
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| {
                            anyhow!(
                                "runtime-filled filter `{}` requires a non-empty correlation",
                                field.name
                            )
                        })?,
                    WriteToolFieldFill::SourceField(source_field) => runtime
                        .source_fields
                        .get(source_field)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow!(
                                "runtime-filled filter `{}` requires source field `{}` in trigger context",
                                field.name,
                                source_field
                            )
                        })?,
                };
                Some(Value::String(filled))
            } else {
                args.get(&field.name).cloned()
            };
            match value {
                Some(Value::Null) | None => {
                    if field.required {
                        bail!(
                            "required filter `{}` missing for tool `{}`",
                            field.name,
                            self.decl.tool_name
                        );
                    }
                }
                Some(Value::String(text)) if text.trim().is_empty() => {
                    if field.required {
                        bail!(
                            "required filter `{}` missing for tool `{}`",
                            field.name,
                            self.decl.tool_name
                        );
                    }
                }
                Some(value) => {
                    if query::is_restricted_field(&self.decl.collection, &field.name) {
                        bail!(
                            "filter `{}` on {:?} is restricted and cannot be queried",
                            field.name,
                            self.decl.collection
                        );
                    }
                    filter.insert(field.name.clone(), json!({ "_eq": value }));
                }
            }
        }
        if filter.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Value::Object(filter)))
        }
    }
}

impl Tool for BoundedQueryTool {
    const NAME: &'static str = PLACEHOLDER_TOOL_NAME;

    type Error = DefraBoundQueryError;
    type Args = BoundedQueryParams;
    type Output = String;

    fn name(&self) -> String {
        self.decl.tool_name.clone()
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = Map::new();
        properties.insert(
            "fields".to_string(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": format!(
                    "Optional subset of allowed fields. Omit to return all of: {}.",
                    self.decl.fields.join(", ")
                )
            }),
        );
        properties.insert(
            "limit".to_string(),
            json!({
                "type": "integer",
                "description": format!(
                    "Maximum rows to return (default {MAX_LIMIT}, capped at {MAX_LIMIT})."
                )
            }),
        );
        let mut required = Vec::new();
        for field in self.model_filter_fields() {
            properties.insert(
                field.name.clone(),
                json!({
                    "type": "string",
                    "description": format!("Filter {} by exact match.", field.name)
                }),
            );
            if field.required {
                required.push(Value::String(field.name.clone()));
            }
        }
        let description = if self.decl.description.trim().is_empty() {
            format!(
                "Read documents from the {} collection. The collection is bound; do not name it.",
                self.decl.collection
            )
        } else {
            self.decl.description.clone()
        };
        ToolDefinition {
            name: self.decl.tool_name.clone(),
            description,
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": required,
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.is_well_formed() {
            return Err(anyhow!(
                "query tool `{}` reached execution with an invalid declaration",
                self.decl.tool_name
            )
            .into());
        }
        let fields = self.resolve_projection(&args.0)?;
        let filter = self.resolve_filter(&args.0)?;
        let limit = self.resolve_limit(&args.0)?;
        let params = DefraQueryParams {
            collection: self.decl.collection.clone(),
            filter,
            fields,
            limit: Some(limit),
        };
        let mut rows = query::execute_query(
            &self.node,
            &params,
            &CollectionScope::restricted(vec![self.decl.collection.clone()]),
        )
        .await?;
        let count = rows.as_array().map(|a| a.len()).unwrap_or(0);
        let total_bytes = serde_json::to_string(&rows).map(|s| s.len()).unwrap_or(0);
        let truncated = truncate_field_strings(&mut rows);
        let mut payload = json!({
            "collection": self.decl.collection,
            "count": count,
            "truncated": truncated,
            "total_bytes": total_bytes,
            "results": rows,
        });
        if truncated {
            payload["truncation_note"] = json!(format!(
                "One or more string fields were truncated to {} bytes; total untruncated result size was {} bytes.",
                MAX_FIELD_STRING_BYTES, total_bytes
            ));
        }
        if count as u32 == limit && limit == MAX_LIMIT {
            payload["limit_note"] = json!(format!(
                "Result set hit the {MAX_LIMIT}-row cap; narrow the filter if more rows exist."
            ));
        }
        serde_json::to_string_pretty(&payload)
            .map_err(|e| DefraBoundQueryError(anyhow!("failed to serialize query results: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::{QueryToolDecl, WriteToolField, WriteToolFieldFill};
    use crate::llm::tool::Tool;

    async fn node_with_findings() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_schema(
            r#"
            type CandidateFinding {
                run_id: String
                finding_id: String
                title: String
            }
        "#,
        )
        .await
        .unwrap();
        node.execute(
            r#"mutation { add_CandidateFinding(input: {
                run_id: "run-42", finding_id: "f1", title: "graphql"
            }) { _docID } }"#,
        )
        .await;
        node.execute(
            r#"mutation { add_CandidateFinding(input: {
                run_id: "other", finding_id: "f2", title: "other-run"
            }) { _docID } }"#,
        )
        .await;
        node
    }

    fn decl() -> QueryToolDecl {
        QueryToolDecl {
            tool_name: "query_candidate_finding".into(),
            collection: "CandidateFinding".into(),
            description: "Load candidate findings for this run.".into(),
            fields: vec!["finding_id".into(), "title".into(), "run_id".into()],
            filter_fields: vec![WriteToolField {
                name: "run_id".into(),
                required: false,
                fill: Some(WriteToolFieldFill::Correlation),
            }],
        }
    }

    #[tokio::test]
    async fn queries_only_the_correlated_run() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(Arc::clone(&node), decl());
        let out =
            crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                None,
                Some("run-42".to_string()),
                Default::default(),
                false,
                async {
                    Tool::call(&tool, BoundedQueryParams(Map::new()))
                        .await
                        .expect("query")
                },
            )
            .await;
        assert!(out.contains("f1"));
        assert!(!out.contains("other-run"));
        assert!(out.contains("\"count\": 1"));
    }

    #[tokio::test]
    async fn hides_filled_filter_and_rejects_model_override() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(node, decl());
        let definition = Tool::definition(&tool, String::new()).await;
        let properties = definition.parameters["properties"].as_object().unwrap();
        assert!(properties.contains_key("fields"));
        assert!(!properties.contains_key("run_id"));
        assert!(!properties.contains_key("collection"));

        let mut args = Map::new();
        args.insert("run_id".into(), json!("model-value"));
        let err = Tool::call(&tool, BoundedQueryParams(args))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("runtime-filled"));
    }

    #[tokio::test]
    async fn rejects_fields_outside_the_allowlist() {
        let node = node_with_findings().await;
        let tool = BoundedQueryTool::new(node, decl());
        let mut args = Map::new();
        args.insert("fields".into(), json!(["_docID"]));
        crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            None,
            tokio_util::sync::CancellationToken::new(),
            None,
            None,
            None,
            Some("run-42".to_string()),
            Default::default(),
            false,
            async {
                let err = Tool::call(&tool, BoundedQueryParams(args))
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("allowlist"));
            },
        )
        .await;
    }
}
