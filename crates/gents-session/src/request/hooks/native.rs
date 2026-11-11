use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use serde_json::Value;

use super::effect::{Access, DocRef, Effect, Manifest, Row, Write};
use super::hook::{Direction, Query};
use super::registry::Registry;
use super::stage::{ApplyFailure, HookError, Host};
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;

/// The lifecycle transaction as a `Host`.
///
/// A row's revision is a digest of the row as it was shown. A write is held
/// only if the document, read again with the same selection, still has that
/// digest, so a plugin cannot overwrite a row it was not shown or one that
/// changed after it decided.
///
/// DefraDB decides document ACP for the transaction's actor when a write
/// executes; `allowed` therefore admits every write and a denied one fails in
/// `apply`. DefraDB has no savepoint, so a plugin can be skipped whole only
/// while none of its effects has written (`ApplyFailure::clean`).
pub(crate) struct TxnHost<'a, 'txn> {
    txn: &'a ConfigApplyTxn<'txn>,
    registry: &'a Registry,
    shown: Mutex<Vec<(DocRef, String)>>,
}

fn identifier(name: &str) -> Result<&str, HookError> {
    let mut characters = name.chars();
    let valid = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|next| next.is_ascii_alphanumeric() || next == '_');
    if valid {
        Ok(name)
    } else {
        Err(HookError::Query(format!("`{name}` is not an identifier")))
    }
}

fn selection(fields: &str) -> Result<String, HookError> {
    if !fields.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || character.is_ascii_whitespace()
            || "_{}".contains(character)
    }) {
        return Err(HookError::Query("selection is not a field list".into()));
    }
    if fields
        .split_ascii_whitespace()
        .any(|field| field == "_docID")
    {
        Ok(fields.to_owned())
    } else {
        Ok(format!("_docID {fields}"))
    }
}

fn literal(value: &Value) -> Result<String, HookError> {
    match value {
        Value::String(text) => Ok(format!("\"{}\"", escape_graphql_string(text))),
        Value::Bool(_) | Value::Number(_) | Value::Null => Ok(value.to_string()),
        Value::Array(_) | Value::Object(_) => Err(HookError::Query(
            "a filter compares a field with a scalar".into(),
        )),
    }
}

fn render(query: &Query, fields: &str) -> Result<String, HookError> {
    let mut arguments = Vec::new();
    if !query.filter.is_empty() {
        let mut filter = Vec::new();
        for (field, value) in &query.filter {
            filter.push(format!(
                "{}: {{ _eq: {} }}",
                identifier(field)?,
                literal(value)?
            ));
        }
        arguments.push(format!("filter: {{ {} }}", filter.join(", ")));
    }
    if !query.order.is_empty() {
        let mut order = Vec::new();
        for (field, direction) in &query.order {
            let direction = match direction {
                Direction::Ascending => "ASC",
                Direction::Descending => "DESC",
            };
            order.push(format!("{{ {}: {direction} }}", identifier(field)?));
        }
        arguments.push(format!("order: [{}]", order.join(", ")));
    }
    if let Some(limit) = query.limit {
        arguments.push(format!("limit: {limit}"));
    }
    let arguments = if arguments.is_empty() {
        String::new()
    } else {
        format!("({})", arguments.join(", "))
    };
    Ok(format!(
        "{{ {}{arguments} {{ {fields} }} }}",
        identifier(&query.collection)?
    ))
}

fn revision(value: &Value) -> u64 {
    let mut hasher = rapidhash::fast::RapidHasher::default();
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

fn returned(response: &Value, root: &str) -> usize {
    response
        .pointer(&format!("/data/{root}"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

impl<'a, 'txn> TxnHost<'a, 'txn> {
    pub(crate) fn new(txn: &'a ConfigApplyTxn<'txn>, registry: &'a Registry) -> Self {
        Self {
            txn,
            registry,
            shown: Mutex::new(Vec::new()),
        }
    }

    async fn rows(&self, query: &Query, fields: &str) -> Result<Vec<Row>, HookError> {
        let response = self.txn.execute(&render(query, fields)?).await?;
        let rows = response
            .pointer(&format!("/data/{}", query.collection))
            .and_then(Value::as_array)
            .ok_or_else(|| HookError::Query(format!("{} read omitted rows", query.collection)))?;
        rows.iter()
            .map(|row| {
                let doc_id = row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| HookError::Query("row has no document ID".into()))?;
                Ok(Row {
                    doc: DocRef::new(query.collection.clone(), doc_id),
                    revision: revision(row),
                    value: row.clone(),
                })
            })
            .collect()
    }

    fn shown_with(&self, doc: &DocRef) -> Option<String> {
        self.shown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .rev()
            .find(|(shown, _)| shown == doc)
            .map(|(_, fields)| fields.clone())
    }

    /// Lean `Hooks.held`.
    async fn held(&self, doc: &DocRef, expected: u64) -> Result<bool, HookError> {
        let Some(fields) = self.shown_with(doc) else {
            return Ok(false);
        };
        let current =
            Query::select(doc.collection.clone(), fields.clone()).eq("_docID", doc.doc_id.clone());
        let rows = self.rows(&current, &fields).await?;
        Ok(matches!(rows.as_slice(), [row] if row.revision == expected))
    }

    async fn write(
        &self,
        plugin: &'static str,
        manifest: &Manifest,
        write: &Write,
    ) -> Result<(), HookError> {
        let doc = write.doc();
        let collection = identifier(&doc.collection)?;
        let schema = manifest.owned(collection).and_then(|owned| owned.schema);
        let target = escape_graphql_string(&doc.doc_id);
        match write {
            Write::Create { value, .. } => {
                let document = format!(
                    "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
                );
                let variables = serde_json::json!({ "input": value });
                let response = match schema {
                    Some(schema) => {
                        self.txn
                            .execute_client_signed_create(&document, &variables, schema)
                            .await?
                    }
                    None => {
                        self.txn
                            .execute_with_variables(&document, &variables)
                            .await?
                    }
                };
                let created = gents_store::graphql::created_doc_id(&response, collection)
                    .map_err(|_| HookError::Conflict { plugin })?;
                if !doc.doc_id.is_empty() && created != doc.doc_id {
                    return Err(HookError::Conflict { plugin });
                }
            }
            Write::Update { value, .. } => {
                let document = format!(
                    "mutation($input: {collection}MutationInputArg!) {{ update_{collection}(filter: {{ _docID: {{ _eq: \"{target}\" }} }}, input: $input) {{ _docID }} }}"
                );
                let variables = serde_json::json!({ "input": value });
                let response = match schema {
                    Some(schema) => {
                        self.txn
                            .execute_client_signed_update(&document, &variables, schema)
                            .await?
                    }
                    None => {
                        self.txn
                            .execute_with_variables(&document, &variables)
                            .await?
                    }
                };
                if returned(&response, &format!("update_{collection}")) != 1 {
                    return Err(HookError::Conflict { plugin });
                }
            }
            Write::Delete { .. } => {
                let document = format!(
                    "mutation {{ delete_{collection}(filter: {{ _docID: {{ _eq: \"{target}\" }} }}) {{ _docID }} }}"
                );
                let response = self.txn.execute(&document).await?;
                if returned(&response, &format!("delete_{collection}")) != 1 {
                    return Err(HookError::Conflict { plugin });
                }
            }
        }
        Ok(())
    }

    async fn effect(&self, manifest: &Manifest, effect: &Effect) -> Result<(), HookError> {
        let plugin = manifest.name;
        match effect {
            Effect::Write(write) => self.write(plugin, manifest, write).await,
            Effect::Core(requested) => {
                let operation = self.registry.operation(&requested.name).ok_or_else(|| {
                    HookError::UnknownOperation {
                        plugin,
                        operation: requested.name.clone(),
                    }
                })?;
                operation
                    .perform(self.txn, &requested.args)
                    .await
                    .map_err(HookError::Operation)
            }
        }
    }
}

#[async_trait::async_trait]
impl Host for TxnHost<'_, '_> {
    async fn select(&self, query: &Query) -> Result<Vec<Row>, HookError> {
        if let Some(links) = &query.revisions {
            identifier(&query.collection)?;
            selection(&query.fields)?;
            for (field, _) in &query.filter {
                identifier(field)?;
            }
            return super::revisions::read(self.txn, query, links)
                .await
                .map_err(HookError::Revisions);
        }
        let fields = selection(&query.fields)?;
        let rows = self.rows(query, &fields).await?;
        let mut shown = self
            .shown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for row in &rows {
            shown.push((row.doc.clone(), fields.clone()));
        }
        Ok(rows)
    }

    async fn allowed(&self, _access: Access, _doc: &DocRef) -> Result<bool, HookError> {
        Ok(true)
    }

    async fn apply(&self, manifest: &Manifest, effects: &[Effect]) -> Result<(), ApplyFailure> {
        let plugin = manifest.name;
        let clean = |error| ApplyFailure { clean: true, error };
        for effect in effects {
            let (doc, expected) = match effect {
                Effect::Write(Write::Update { doc, expected, .. })
                | Effect::Write(Write::Delete { doc, expected }) => (doc, *expected),
                Effect::Write(Write::Create { .. }) | Effect::Core(_) => continue,
            };
            if !self.held(doc, expected).await.map_err(clean)? {
                return Err(clean(HookError::Conflict { plugin }));
            }
        }
        let before = self.txn.affected_documents();
        for effect in effects {
            if let Err(error) = self.effect(manifest, effect).await {
                return Err(ApplyFailure {
                    clean: self.txn.affected_documents() == before,
                    error,
                });
            }
        }
        Ok(())
    }
}
