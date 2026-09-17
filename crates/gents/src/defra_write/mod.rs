//! Declarative, schema-bounded single-collection write tool.
//!
//! The declaration owns the tool name and permitted fields; DefraDB owns their
//! types. Each call writes one document through the configuration transaction
//! owner and returns its canonical receipt, including runtime-filled metadata.

use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde_json::{json, Map, Value};

use crate::document_config::WriteToolDecl;

mod input;

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
        self.ensure_well_formed().is_ok()
    }

    fn ensure_well_formed(&self) -> Result<()> {
        if self.decl.collection == crate::mailbox::MAILBOX_COLLECTION {
            bail!(
                "MailboxItem requires the dedicated stamped mailbox tool and cannot use BoundedWriteTool"
            );
        }
        self.decl
            .validate()
            .map_err(|error| anyhow!("invalid bounded write tool declaration: {error}"))?;
        self.field_types()?;
        Ok(())
    }

    fn field_types(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let collection = self
            .node
            .get_collection(&self.decl.collection)?
            .ok_or_else(|| anyhow!("collection `{}` is not available", self.decl.collection))?;
        self.decl
            .fields
            .iter()
            .map(|field| {
                let schema_field = collection
                    .fields
                    .iter()
                    .find(|f| f.name == field.name)
                    .ok_or_else(|| {
                        anyhow!(
                            "field `{}` is absent from `{}`",
                            field.name,
                            self.decl.collection
                        )
                    })?;
                let schema = schema_field.kind.graphql_type_name();
                input::parameters(schema)?;
                Ok((field.name.clone(), schema.to_owned()))
            })
            .collect()
    }

    fn build_mutation(&self, args: &Map<String, Value>) -> Result<String> {
        // `ensure_well_formed` delegates to `WriteToolDecl::validate`, which
        // validates the collection and every field name as GraphQL identifiers
        // (the centralized identifier-validation boundary), so bare-identifier
        // interpolation below is safe without a second per-site check here.
        self.ensure_well_formed()?;
        let types = self.field_types()?;

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
            input::validate_input(
                &types[&field.name],
                field.required,
                field.fill.is_some(),
                args.get(&field.name),
            )
            .with_context(|| {
                format!(
                    "invalid field `{}` for tool `{}`",
                    field.name, self.decl.tool_name
                )
            })?;
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
                    filled = Value::String(fill.resolve(&field.name)?);
                    &filled
                }
            };
            let literal = input::literal(&types[&field.name], value)
                .with_context(|| format!("invalid field `{}`", field.name))?;
            input_parts.push(format!("{}: {}", field.name, literal));
        }

        Ok(format!(
            "mutation {{ add_{collection}(input: {{ {input} }}) {{ _docID {fields} }} }}",
            collection = self.decl.collection,
            input = input_parts.join(", "),
            fields = self
                .decl
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(" "),
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
        let types = match self.field_types() {
            Ok(types) => types,
            Err(error) => {
                tracing::error!(tool = %self.decl.tool_name, %error, "bounded writer schema unavailable");
                return ToolDefinition {
                    name: self.decl.tool_name.clone(),
                    description: "Unavailable: collection schema could not be resolved.".into(),
                    parameters: json!({"type":"object", "properties":{}, "additionalProperties":false}),
                };
            }
        };
        for field in &self.decl.fields {
            if field.fill.is_some() {
                continue;
            }
            properties.insert(
                field.name.clone(),
                input::parameters(&types[&field.name])
                    .expect("field_types validates supported schemas"),
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
                "additionalProperties": false,
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mutation = self.build_mutation(&args.0)?;

        let resp = crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "tool.defra_write",
            &mutation,
        )
        .await?;

        let document = resp
            .data
            .as_ref()
            .and_then(|data| data.get(format!("add_{}", self.decl.collection)))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .ok_or_else(|| anyhow!("write returned no canonical document"))?;
        let doc_id = document
            .get("_docID")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("write to {:?} returned no _docID", self.decl.collection))?;
        Ok(
            json!({"collection":self.decl.collection, "document_id":doc_id,
            "document":document})
            .to_string(),
        )
    }
}

#[cfg(test)]
mod tests;
