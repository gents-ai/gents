use anyhow::{anyhow, bail, Context, Result};
use gents_loop::tool::ToolDefinition;
use serde_json::{json, Map, Value};

use gents_documents::WriteToolDecl;
use gents_store::config_client::ConfigAccess;

mod input;

pub(crate) use gents_store::config_client::schema_introspection::parameters as field_parameters;
pub use input::validate_input as validate_field_input;

pub fn field_literal(schema: &str, value: &Value) -> Result<String> {
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
    surface_scope: Option<gents_documents::DatastoreSurfaceScope>,
    client_schema: Option<String>,
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
            surface_scope: None,
            client_schema: None,
        }
    }

    pub fn declared_by(mut self, surface_id: Option<String>) -> Self {
        self.surface_id = surface_id;
        self
    }

    pub fn with_surface_owner(mut self, owner: String) -> Self {
        self.surface_scope = Some(gents_documents::DatastoreSurfaceScope::SelectedAgent(owner));
        self
    }

    pub(crate) fn with_visible_surface_owner(mut self, owner: String) -> Self {
        self.surface_scope = Some(gents_documents::DatastoreSurfaceScope::VisibleCaller(owner));
        self
    }

    pub(crate) fn with_ux_client_schema(mut self, schema: String) -> Self {
        self.client_schema = Some(schema);
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
        if self.decl.collection == gents_protocol::row::MAILBOX_COLLECTION {
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
                field_parameters(schema)?;
                Ok((field.name.clone(), schema.to_owned()))
            })
            .collect()
    }

    fn build_input(
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
                    gents_documents::undeclared_field_refusal(
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
                    gents_documents::runtime_filled_refusal(
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

        Ok(format!("{{ {} }}", input_parts.join(", ")))
    }

    fn selection(&self) -> String {
        self.decl
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The filter an upsert finds its document by: each key field equal to
    /// the value this call writes to it.
    fn key_filter(
        &self,
        keys: &[String],
        args: &Map<String, Value>,
        types: &std::collections::BTreeMap<String, String>,
    ) -> Result<String> {
        let mut parts = Vec::with_capacity(keys.len());
        for key in keys {
            let field = self
                .decl
                .fields
                .iter()
                .find(|field| &field.name == key)
                .with_context(|| format!("key field `{key}` is not declared"))?;
            let value = match &field.fill {
                None => args.get(key).filter(|value| !value.is_null()).cloned(),
                Some(fill) => fill.resolve_optional(key)?.map(Value::String),
            }
            .ok_or_else(|| {
                anyhow!(
                    "tool `{}` finds its document by {}; give `{key}` a value",
                    self.decl.tool_name,
                    keys.join(", ")
                )
            })?;
            parts.push(format!(
                "{key}: {{_eq: {}}}",
                input::literal(&types[key], &value)?
            ));
        }
        Ok(format!("{{ {} }}", parts.join(", ")))
    }

    /// The document an upsert updates: none when no document has the key
    /// values, the one that has them when every commit of it was signed by
    /// `writer`, and a refusal otherwise.
    async fn upsert_target(
        &self,
        txn: &gents_store::config_client::ConfigApplyTxn<'_>,
        filter: &str,
        writer: Option<&str>,
    ) -> Result<Option<String>> {
        let collection = &self.decl.collection;
        let response = txn
            .execute(&format!(
                "{{ {collection}(filter: {filter}) {{ _docID }} }}"
            ))
            .await?;
        let ids = response["data"][collection]
            .as_array()
            .context("DefraDB returned no collection result")?
            .iter()
            .map(|row| row["_docID"].as_str().context("row has no document ID"))
            .collect::<Result<Vec<_>>>()?;
        match ids.as_slice() {
            [] => Ok(None),
            [id] => {
                let signers = gents_store::identity::document_signers(txn, &ids).await?;
                let signer = signers.get(*id).cloned().flatten();
                if signer.is_none() || signer.as_deref() != writer {
                    bail!(
                        "tool `{}` will not update {collection} {id}: {}; an upsert updates only a document its writer signed",
                        self.decl.tool_name,
                        match signer {
                            Some(did) => format!("it was written by {did}"),
                            None => "no single identity signed it".to_owned(),
                        }
                    );
                }
                Ok(Some((*id).to_owned()))
            }
            many => bail!(
                "tool `{}` will not update: {} {collection} documents have these key values; an upsert updates exactly one",
                self.decl.tool_name,
                many.len()
            ),
        }
    }
}

impl gents_loop::tool::Tool for BoundedWriteTool {
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
                field_parameters(&types[&field.name])
                    .expect("field_types validates supported schemas"),
            );
            if field.required {
                required.push(Value::String(field.name.clone()));
            }
        }

        ToolDefinition {
            name: self.decl.tool_name.clone(),
            description: match &self.decl.key_fields {
                Some(keys) => format!(
                    "Creates the {collection} record whose {keys} equal the input's, or updates it when you wrote it; this can trigger automation watching that collection. {description}",
                    collection = self.decl.collection,
                    keys = keys.join(", "),
                    description = self.decl.description
                ),
                None => format!(
                    "Creates a new {} record; this can trigger automation watching that collection. {}",
                    self.decl.collection, self.decl.description
                ),
            },
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
        let input = self.build_input(&args.0, &types)?;
        let key_filter = self
            .decl
            .key_fields
            .as_deref()
            .map(|keys| self.key_filter(keys, &args.0, &types))
            .transpose()?;
        if self.client_schema.is_some() {
            if !matches!(self.access, ConfigAccess::Graphql(_)) || self.surface_scope.is_none() {
                return Err(
                    anyhow!("UX client signing requires a scoped remote transaction").into(),
                );
            }
            if self.decl.fields.iter().any(|field| field.fill.is_some()) {
                return Err(anyhow!("UX client writes cannot use runtime-filled fields").into());
            }
        }
        let variables = json!({"input": &args.0});
        let collection = &self.decl.collection;
        let selection = self.selection();
        let create =
            format!("mutation {{ add_{collection}(input: {input}) {{ _docID {selection} }} }}");

        let (resp, updated) = if self.surface_scope.is_some() || key_filter.is_some() {
            self.access
                .transact_write("tool.defra_write", |txn| {
                    let create = &create;
                    let input = &input;
                    let selection = &selection;
                    let variables = &variables;
                    let key_filter = &key_filter;
                    Box::pin(async move {
                        if let Some(scope) = &self.surface_scope {
                            let surface = self
                                .surface_id
                                .as_deref()
                                .context("datastore source missing")?;
                            scope
                                .revalidate(
                                    txn,
                                    surface,
                                    &gents_documents::SurfaceToolDecl::Create(self.decl.clone()),
                                )
                                .await?;
                        }
                        let writer = if self.client_schema.is_some() {
                            txn.client_held_signer_did()
                        } else {
                            txn.actor_did()
                        };
                        let target = match key_filter {
                            Some(filter) => self.upsert_target(txn, filter, writer).await?,
                            None => None,
                        };
                        let response = match (&self.client_schema, &target) {
                            (Some(schema), None) => {
                                let document = format!(
                                    "mutation($input: {collection}MutationInputArg!) {{ add_{collection}(input: $input) {{ _docID {selection} }} }}"
                                );
                                txn.execute_client_signed_create(&document, variables, schema)
                                    .await?
                            }
                            (Some(schema), Some(doc_id)) => {
                                let document = format!(
                                    "mutation($input: {collection}MutationInputArg!) {{ update_{collection}(docID: \"{}\", input: $input) {{ _docID {selection} }} }}",
                                    gents_store::graphql::escape_graphql_string(doc_id)
                                );
                                txn.execute_client_signed_update(&document, variables, schema)
                                    .await?
                            }
                            (None, None) => txn.execute(create).await?,
                            (None, Some(doc_id)) => {
                                txn.execute(&format!(
                                    "mutation {{ update_{collection}(docID: \"{}\", input: {input}) {{ _docID {selection} }} }}",
                                    gents_store::graphql::escape_graphql_string(doc_id)
                                ))
                                .await?
                            }
                        };
                        written_document(&response, collection, target.is_some())?;
                        Ok::<_, anyhow::Error>((response, target.is_some()))
                    })
                })
                .await?
        } else {
            let response = self
                .access
                .write("tool.defra_write", &create)
                .await
                .map_err(anyhow::Error::from)?;
            (response, false)
        };

        let (doc_id, document) = written_document(&resp, collection, updated)?;
        let mut output =
            json!({"collection": collection, "document_id": doc_id, "document": document});
        let mut order = vec!["document", "document_id", "collection"];
        if self.decl.key_fields.is_some() {
            output["written"] = json!(if updated { "updated" } else { "created" });
            order.insert(0, "written");
        }
        Ok(gents_session::session::output_render::render(
            &output, &order,
        )?)
    }
}

/// The one document a create or an update returned, with its ID.
fn written_document(response: &Value, collection: &str, updated: bool) -> Result<(String, Value)> {
    let field = if updated {
        format!("update_{collection}")
    } else {
        format!("add_{collection}")
    };
    let rows = gents_store::graphql::mutation_documents(response, &field)?.unwrap_or_default();
    let [document] = rows else {
        bail!(
            "write of {collection} returned {} documents, not one",
            rows.len()
        );
    };
    let doc_id = document["_docID"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .with_context(|| format!("write of {collection} returned no document ID"))?;
    Ok((doc_id.to_owned(), document.clone()))
}

/// How an agent comes to write application collection(s) through the `write`
/// tool, whose grant is the exact names in its Tools'
/// `datastore.write_collections`. `datastore` is the caller's current
/// datastore group when it could be read: a `set` replaces a whole group, so
/// the call sends the group back with the names added. A caller without the
/// `tools` category of `config` cannot make the call and is told who can.
pub(crate) fn write_grant_guidance(
    holds_tools: bool,
    datastore: Option<&gents_documents::DatastoreTools>,
    collections: &[String],
) -> Value {
    let names = collections.join(", ");
    let granted = |name: &String| {
        datastore
            .and_then(|group| group.write_collections.as_ref())
            .is_some_and(|granted| granted.contains(name))
    };
    if !collections.is_empty() && collections.iter().all(granted) {
        return json!({
            "writable": true,
            "how": format!("Your Tools already grant the write tool {names}."),
        });
    }
    if !holds_tools {
        return json!({
            "writable": false,
            "how": format!("Writes go through the write tool, which takes the exact collection names in your Tools' datastore.write_collections; {names} is not there and you do not hold the tools category of config, so you cannot add it. The node's User, or an agent that configures your Tools (the Engineer), adds it with config {{\"argv\":[\"tools\",\"edit\"],\"options\":{{\"agent\":YOUR_AGENT_DID}},\"set\":{{\"datastore\":{{...}}}}}}."),
        });
    }
    let how = format!("Writes go through the write tool, which takes the exact collection names in your Tools' datastore.write_collections; {names} is not there yet. Add it with next_call, then write in a fresh request.");
    match datastore {
        Some(group) => {
            let mut group = group.clone();
            let granted = group.write_collections.get_or_insert_with(Vec::new);
            for name in collections {
                if !granted.contains(name) {
                    granted.push(name.clone());
                }
            }
            json!({
                "writable": false,
                "how": how,
                "next_call": {"tool": "config", "args": {"argv": ["tools", "edit"], "set": {"datastore": group}}},
            })
        }
        None => json!({
            "writable": false,
            "how": format!("{how} A set replaces the whole datastore group: read yours with config [\"tools\",\"get\"] and send it back with write_collections extended."),
            "next_call": {"tool": "config", "args": {"argv": ["tools", "edit"], "set": {"datastore": {"write_collections": collections}}}},
        }),
    }
}

#[cfg(test)]
mod tests;
