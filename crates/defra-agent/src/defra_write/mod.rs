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
//! distinct tools backed by the same type. `rig`'s [`rig::tool::Tool`] trait
//! supports this directly: it requires a `const NAME` but also exposes a
//! `fn name(&self) -> String` that defaults to that const — and which we
//! override here to return `self.decl.tool_name`. The blanket
//! `impl<T: Tool> ToolDyn for T` in `rig` forwards `name()`, so dynamic dispatch
//! (B4's job) sees the per-instance name with no extra machinery.
//!
//! The `const NAME` on this impl is therefore a *placeholder* that is never the
//! advertised identity; per-instance identity always comes from
//! [`Tool::name`]/[`Tool::definition`]'s `name`.
//!
//! The alternative — implementing `rig::tool::ToolDyn` by hand like
//! `toolset::cli_tool::CliTool` — also yields a runtime `name()`, but its
//! `call(&self, args: String)` signature is the wrong shape for the typed,
//! directly-callable contract this task's tests drive. Overriding `Tool::name`
//! gives both the typed `call(Args)` and the dynamic name, so it is the better
//! fit.

use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use defra_node::EmbeddedNode;
use rig::completion::ToolDefinition;
use serde_json::{json, Map, Value};

use crate::document_config::WriteToolDecl;
use crate::graphql::escape_graphql_string;

/// Placeholder const for the `rig::tool::Tool` impl. Bounded write tools are
/// named per declaration (see module docs); the real, advertised name always
/// comes from [`BoundedWriteTool::name`] / [`BoundedWriteTool::definition`],
/// never from this const.
const PLACEHOLDER_TOOL_NAME: &str = "defra_write";

/// Error wrapper mirroring [`crate::defra_query::DefraQueryError`]: render the
/// full anyhow chain to the model.
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

/// The write contract is intentionally free-form: a flat map of declared
/// field name → value. The set of permitted keys and which are required is
/// fixed by the tool's [`WriteToolDecl`], not by this type, so a single struct
/// serves every declaration. Validation against the declaration happens in
/// [`BoundedWriteTool::call`].
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(transparent)]
pub struct BoundedWriteParams(pub Map<String, Value>);

/// A schema-bounded, single-collection document writer. One instance per
/// [`WriteToolDecl`].
#[derive(Clone)]
pub struct BoundedWriteTool {
    node: Arc<EmbeddedNode>,
    decl: WriteToolDecl,
}

impl BoundedWriteTool {
    /// Build a writer for one declaration.
    ///
    /// A well-formed declaration must name both a tool and a collection. An
    /// empty `tool_name` or `collection` is a config/programming error (it would
    /// otherwise silently produce a tool that writes to `""`). Construction does
    /// not panic so callers can surface the error through the normal tool-call
    /// path; instead every write is gated by [`Self::ensure_well_formed`], and
    /// [`Self::is_well_formed`] lets B4's registration skip/reject a malformed
    /// declaration up front.
    pub fn new(node: Arc<EmbeddedNode>, decl: WriteToolDecl) -> Self {
        Self { node, decl }
    }

    /// True when the declaration names both a tool and a collection. B4's
    /// registration can use this to reject a malformed `write_tools` entry
    /// before it ever reaches the live toolset.
    pub fn is_well_formed(&self) -> bool {
        self.decl.is_well_formed()
    }

    /// Reject a structurally invalid declaration so we never emit a mutation
    /// against an empty collection name.
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

    /// Validate the supplied args against the declaration and build the
    /// `mutation { add_<Collection>(input: { ... }) { _docID } }` string.
    ///
    /// - any key not declared by the tool is rejected;
    /// - any declared `required` field absent from the args is rejected;
    /// - every value is escaped through [`escape_graphql_string`] (string values
    ///   verbatim; non-string JSON values serialized to their compact string
    ///   form first, then escaped) — for v1 every field is written as a string.
    fn build_mutation(&self, args: &Map<String, Value>) -> Result<String> {
        self.ensure_well_formed()?;

        // 1. No undeclared keys.
        for key in args.keys() {
            let declared = self.decl.fields.iter().any(|f| &f.name == key);
            if !declared {
                bail!(
                    "field `{key}` not permitted by tool `{}`",
                    self.decl.tool_name
                );
            }
        }

        // 2. All required declared fields present.
        for field in &self.decl.fields {
            if field.required && !args.contains_key(&field.name) {
                bail!(
                    "required field `{}` missing for tool `{}`",
                    field.name,
                    self.decl.tool_name
                );
            }
        }

        // 3. Build the input object, preserving declared field order for a
        //    stable, readable mutation. Skip declared-but-absent optional fields.
        let mut input_parts = Vec::new();
        for field in &self.decl.fields {
            let Some(value) = args.get(&field.name) else {
                continue;
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

impl rig::tool::Tool for BoundedWriteTool {
    // Placeholder only — see module docs and `PLACEHOLDER_TOOL_NAME`. Real
    // identity is the per-instance `name()` / `definition().name` below.
    const NAME: &'static str = PLACEHOLDER_TOOL_NAME;

    type Error = DefraWriteError;
    type Args = BoundedWriteParams;
    type Output = String;

    /// Per-instance name: the declaration's `tool_name`, not [`Self::NAME`].
    fn name(&self) -> String {
        self.decl.tool_name.clone()
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for field in &self.decl.fields {
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

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            return Err(DefraWriteError(anyhow!(
                "write to {:?} via tool `{}` failed: {:?}",
                self.decl.collection,
                self.decl.tool_name,
                resp.errors
            )));
        }

        let doc_id = extract_doc_id(resp.data.as_ref(), &self.decl.collection)
            .ok_or_else(|| anyhow!("write to {:?} returned no _docID", self.decl.collection))?;

        Ok(format!("created {} {}", self.decl.collection, doc_id))
    }
}

/// Pull the `_docID` out of an `add_<Collection>` mutation response, handling
/// both the object form (`{ _docID }`) and the array-of-rows form
/// (`[{ _docID }]`) that dynamically-added collections return. Mirrors the
/// extraction in `tests/event_trigger_e2e.rs::write_webhook_event`.
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
