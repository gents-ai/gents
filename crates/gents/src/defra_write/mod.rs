use crate::inference::llm::tool::ToolDefinition;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::config_client::ConfigAccess;
use crate::documents::WriteToolDecl;

mod input;

pub use input::can_hold_canonical_count;
pub(crate) use input::parameters as field_parameters;
pub(crate) use input::validate_input as validate_field_input;

pub(crate) fn field_literal(schema: &str, value: &Value) -> Result<String> {
    input::literal(schema, value)
}

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
    access: ConfigAccess,
    decl: WriteToolDecl,
    surface_id: Option<String>,
}

impl BoundedWriteTool {
    /// `access` is where the document is written and whose principal
    /// writes it: the embedded node, or a served node's GraphQL endpoint
    /// acting as its DID.
    pub fn new(access: ConfigAccess, decl: WriteToolDecl) -> Self {
        Self {
            access,
            decl,
            surface_id: None,
        }
    }

    pub fn declared_by(mut self, surface_id: Option<String>) -> Self {
        self.surface_id = surface_id;
        self
    }

    pub fn is_well_formed(&self) -> bool {
        self.ensure_well_formed().is_ok()
    }

    /// The declaration's own rules, plus the collection's field types when
    /// the embedded node is at hand. A GraphQL endpoint's schema is read
    /// over HTTP at the call, so for it this judges the declaration alone.
    pub(crate) fn ensure_well_formed(&self) -> Result<()> {
        self.ensure_declaration()?;
        if matches!(self.access, ConfigAccess::Local(_)) {
            self.field_types()?;
        }
        Ok(())
    }

    fn ensure_declaration(&self) -> Result<()> {
        if self.decl.collection == crate::session::mailbox::MAILBOX_COLLECTION {
            bail!(
                "MailboxItem requires the dedicated stamped mailbox tool and cannot use BoundedWriteTool"
            );
        }
        self.decl
            .validate()
            .map_err(|error| anyhow!("invalid bounded write tool declaration: {error}"))
    }

    /// The field types read from the embedded node, without I/O.
    fn field_types(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let ConfigAccess::Local(local) = &self.access else {
            bail!(
                "collection `{}` is read over HTTP at the call",
                self.decl.collection
            );
        };
        let collection = local
            .node()
            .get_collection(&self.decl.collection)?
            .ok_or_else(|| anyhow!("collection `{}` is not available", self.decl.collection))?;
        self.field_types_of(&collection)
    }

    /// The field types from wherever `access` reads the schema.
    async fn resolved_field_types(&self) -> Result<std::collections::BTreeMap<String, String>> {
        if matches!(self.access, ConfigAccess::Local(_)) {
            return self.field_types();
        }
        let version = self
            .access
            .collection_version(&self.decl.collection)
            .await?
            .ok_or_else(|| anyhow!("collection `{}` is not available", self.decl.collection))?;
        let collection: ::schema::CollectionVersion = serde_json::from_value(version)
            .with_context(|| format!("decoding the `{}` schema", self.decl.collection))?;
        self.field_types_of(&collection)
    }

    fn field_types_of(
        &self,
        collection: &::schema::CollectionVersion,
    ) -> Result<std::collections::BTreeMap<String, String>> {
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

    fn build_mutation(
        &self,
        args: &Map<String, Value>,
        types: &std::collections::BTreeMap<String, String>,
    ) -> Result<String> {
        self.ensure_declaration()?;

        for key in args.keys() {
            let field = self.decl.fields.iter().find(|field| &field.name == key);
            if field.is_none() {
                bail!(
                    "{}",
                    crate::documents::undeclared_field_refusal(
                        "field",
                        key,
                        &self.decl.tool_name,
                        self.surface_id.as_deref(),
                        &self
                            .decl
                            .fields
                            .iter()
                            .filter(|field| field.fill.is_none())
                            .map(|field| field.name.as_str())
                            .collect::<Vec<_>>(),
                    )
                );
            }
            if let Some(fill) = field.and_then(|field| field.fill.as_ref()) {
                bail!(
                    "{}",
                    crate::documents::runtime_filled_refusal(
                        "field",
                        key,
                        &self.decl.tool_name,
                        self.surface_id.as_deref(),
                        fill,
                    )
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
                    let Some(value) = fill.resolve_optional(&field.name)? else {
                        continue;
                    };
                    filled = Value::String(value);
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

impl crate::inference::llm::tool::Tool for BoundedWriteTool {
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
        let types = match self.resolved_field_types().await {
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
            description: format!(
                "Creates a new {} record; this can trigger automation watching that collection. {}",
                self.decl.collection, self.decl.description
            ),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.ensure_declaration()?;
        let types = self.resolved_field_types().await?;
        let mutation = self.build_mutation(&args.0, &types)?;

        let resp = self
            .access
            .write("tool.defra_write", &mutation)
            .await
            .map_err(anyhow::Error::from)?;

        let document = resp
            .get("data")
            .and_then(|data| data.get(format!("add_{}", self.decl.collection)))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .ok_or_else(|| anyhow!("write returned no canonical document"))?;
        let doc_id = document
            .get("_docID")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("write to {:?} returned no _docID", self.decl.collection))?;
        Ok(crate::tool_output::render(
            &json!({"collection":self.decl.collection, "document_id":doc_id,
                "document":document}),
            &["document", "document_id", "collection"],
        )?)
    }
}

#[cfg(test)]
mod tests;
