//! Declarative, schema-bounded single-collection write tool.
//!
//! `defra_write` is the write-side sibling of [`crate::defra_query`]. Where
//! `DefraQueryTool` is one read tool that can read any (in-scope) collection, a
//! [`BoundedWriteTool`] is the opposite shape: each instance is locked to a
//! single [`WriteToolDecl`] — one collection and one fixed field set — and
//! writes exactly one validated document per call. The agent never names the
//! collection or invents fields; the declaration is the contract.
//!
//! ## Dynamic per-instance tool name
//!
//! Unlike `DefraQueryTool` (a single tool with a shared
//! `const NAME: &str = "defra_query"`), bounded write tools are *named per
//! declaration*: a `request_action` decl and a `record_finding` decl are
//! distinct tools backed by the same type. The native [`crate::llm::tool::Tool`] trait
//! supports this directly: it requires a `const NAME` but also exposes a
//! `fn name(&self) -> String` that defaults to that const — and which we
//! override here to return `self.decl.tool_name`. The blanket
//! `impl<T: Tool> ToolDyn for T` in `crate::llm::tool` forwards `name()`, so dynamic dispatch
//! (B4's job) sees the per-instance name with no extra machinery.
//!
//! The `const NAME` on this impl is therefore a *placeholder* that is never the
//! advertised identity; per-instance identity always comes from
//! [`Tool::name`]/[`Tool::definition`]'s `name`.
//!
//! The alternative — implementing `ToolDyn` by hand like
//! `toolset::cli_tool::CliTool` — also yields a runtime `name()`, but its
//! `call(&self, args: String)` signature is the wrong shape for the typed,
//! directly-callable contract this task's tests drive. Overriding `Tool::name`
//! gives both the typed `call(Args)` and the dynamic name, so it is the better
//! fit.

use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use anyhow::{anyhow, bail, Result};
use defra_node::EmbeddedNode;
use serde_json::{json, Map, Value};

use crate::document_config::{WriteToolDecl, WriteToolFieldFill};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

const PLACEHOLDER_TOOL_NAME: &str = "defra_write";

#[derive(Debug)]
pub struct DefraWriteError(anyhow::Error);

impl std::fmt::Display for DefraWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for DefraWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for DefraWriteError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(transparent)]
pub struct BoundedWriteParams(pub Map<String, Value>);

#[derive(Clone)]
pub struct BoundedWriteTool {
    node: Arc<EmbeddedNode>,
    decl: WriteToolDecl,
}

impl BoundedWriteTool {
    pub fn new(node: Arc<EmbeddedNode>, decl: WriteToolDecl) -> Self {
        Self { node, decl }
    }

    pub fn is_well_formed(&self) -> bool {
        self.decl.is_well_formed()
    }

    fn ensure_well_formed(&self) -> Result<()> {
        if self.decl.tool_name.trim().is_empty() {
            bail!("bounded write tool declaration has an empty tool_name");
        }
        if self.decl.collection.trim().is_empty() {
            bail!(
                "bounded write tool {:?} has an empty collection and cannot write",
                self.decl.tool_name
            );
        }
        Ok(())
    }

    fn build_mutation(&self, args: &Map<String, Value>) -> Result<String> {
        self.ensure_well_formed()?;

        for key in args.keys() {
            let field = self.decl.fields.iter().find(|field| &field.name == key);
            if field.is_none() {
                bail!(
                    "field `{key}` not permitted by tool `{}`",
                    self.decl.tool_name
                );
            }
            if field.is_some_and(|field| field.fill.is_some()) {
                bail!(
                    "field `{key}` is runtime-filled and must not be supplied to tool `{}`",
                    self.decl.tool_name
                );
            }
        }

        for field in &self.decl.fields {
            if field.fill.is_none() && field.required && !args.contains_key(&field.name) {
                bail!(
                    "required field `{}` missing for tool `{}`",
                    field.name,
                    self.decl.tool_name
                );
            }
        }

        let mut input_parts = Vec::new();
        for field in &self.decl.fields {
            let filled;
            let value = match &field.fill {
                None => {
                    let Some(value) = args.get(&field.name) else {
                        continue;
                    };
                    value
                }
                Some(fill) => {
                    let runtime =
                        crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
                            .ok_or_else(|| {
                                anyhow!(
                            "runtime-filled field `{}` requires an AgentRequest trigger context",
                            field.name
                        )
                            })?;
                    let value = match fill {
                        WriteToolFieldFill::Correlation => runtime
                            .correlation
                            .filter(|value| !value.trim().is_empty())
                            .ok_or_else(|| {
                                anyhow!(
                                    "runtime-filled field `{}` requires a non-empty correlation",
                                    field.name
                                )
                            })?,
                        WriteToolFieldFill::SourceField(source_field) => runtime
                            .source_fields
                            .get(source_field)
                            .cloned()
                            .ok_or_else(|| anyhow!(
                                "runtime-filled field `{}` requires source field `{}` in trigger context",
                                field.name,
                                source_field
                            ))?,
                    };
                    filled = Value::String(value);
                    &filled
                }
            };
            let raw = match value {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            let escaped = escape_graphql_string(&raw);
            input_parts.push(format!("{}: \"{}\"", field.name, escaped));
        }

        Ok(format!(
            "mutation {{ add_{collection}(input: {{ {input} }}) {{ _docID }} }}",
            collection = self.decl.collection,
            input = input_parts.join(", "),
        ))
    }
}

impl crate::llm::tool::Tool for BoundedWriteTool {
    const NAME: &'static str = PLACEHOLDER_TOOL_NAME;

    type Error = DefraWriteError;
    type Args = BoundedWriteParams;
    type Output = String;

    fn name(&self) -> String {
        self.decl.tool_name.clone()
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for field in &self.decl.fields {
            if field.fill.is_some() {
                continue;
            }
            properties.insert(
                field.name.clone(),
                json!({
                    "type": "string",
                    "description": format!("Value for the `{}` field.", field.name),
                }),
            );
            if field.required {
                required.push(Value::String(field.name.clone()));
            }
        }

        ToolDefinition {
            name: self.decl.tool_name.clone(),
            description: self.decl.description.clone(),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": required,
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mutation = self.build_mutation(&args.0)?;

        let operation = format!(
            "write to {:?} via tool `{}`",
            self.decl.collection, self.decl.tool_name
        );
        let resp = graphql_with_transaction_retry(&self.node, &mutation, &operation).await?;

        let doc_id = extract_doc_id(resp.data.as_ref(), &self.decl.collection)
            .ok_or_else(|| anyhow!("write to {:?} returned no _docID", self.decl.collection))?;

        Ok(format!("created {} {}", self.decl.collection, doc_id))
    }
}

fn extract_doc_id(data: Option<&Value>, collection: &str) -> Option<String> {
    let data = data?;
    let add_key = format!("add_{collection}");
    let create_key = format!("create_{collection}");
    let field = data.get(&add_key).or_else(|| data.get(&create_key))?;

    field
        .get("_docID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            field
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

#[cfg(test)]
mod tests;
