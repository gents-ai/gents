use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

use crate::store::graphql::escape_graphql_string;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaField {
    pub name: String,
    pub type_name: String,
}

impl SchemaField {
    pub fn named_type(&self) -> &str {
        self.type_name.trim_end_matches('!')
    }
}

#[derive(Debug, Clone)]
pub struct CollectionSchema {
    pub fields: Vec<SchemaField>,
}

impl CollectionSchema {
    pub(crate) fn field_names(&self) -> BTreeSet<&str> {
        self.fields.iter().map(|f| f.name.as_str()).collect()
    }

    /// Fields a reader may name: `restricted` is the caller's field policy.
    pub(crate) fn visible_fields<F>(&self, restricted: F) -> Vec<&SchemaField>
    where
        F: Fn(&str) -> bool,
    {
        self.fields
            .iter()
            .filter(|f| !restricted(&f.name))
            .filter(|f| !is_aggregate_pseudo_field(&f.name))
            .filter(|f| !f.name.starts_with('_') || f.name == "_docID")
            .collect()
    }
}

pub(crate) fn is_aggregate_pseudo_field(name: &str) -> bool {
    matches!(
        name,
        "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "GROUP" | "SIMILARITY" | "BM25"
    )
}

pub(crate) fn validate_identifier(name: &str) -> Result<()> {
    Ok(crate::store::graphql::validate_graphql_name(name)?)
}

const TYPE_FIELDS: &str = "fields { name type { name kind ofType { name kind } } }";

pub fn introspection_query(collection: &str) -> Result<String> {
    validate_identifier(collection).map_err(|e| anyhow!("invalid collection name: {e}"))?;
    Ok(format!(
        r#"{{ __type(name: "{name}") {{ {TYPE_FIELDS} }} }}"#,
        name = escape_graphql_string(collection)
    ))
}

pub fn introspection_query_many(collections: &[&str]) -> Result<String> {
    let mut query = String::from("{");
    for (index, collection) in collections.iter().enumerate() {
        validate_identifier(collection).map_err(|e| anyhow!("invalid collection name: {e}"))?;
        query.push_str(&format!(
            r#" c{index}: __type(name: "{name}") {{ {TYPE_FIELDS} }}"#,
            name = escape_graphql_string(collection)
        ));
    }
    query.push_str(" }");
    Ok(query)
}

pub fn parse_collection_schema(data: Option<&Value>) -> Option<CollectionSchema> {
    parse_type(data?.get("__type")?)
}

pub fn parse_collection_schemas(
    data: Option<&Value>,
    count: usize,
) -> Vec<Option<CollectionSchema>> {
    (0..count)
        .map(|index| {
            data.and_then(|data| data.get(format!("c{index}")))
                .and_then(parse_type)
        })
        .collect()
}

fn parse_type(ty: &Value) -> Option<CollectionSchema> {
    let fields = ty.get("fields")?.as_array()?;
    Some(CollectionSchema {
        fields: fields
            .iter()
            .filter_map(|f| {
                let name = f.get("name")?.as_str()?.to_string();
                Some(SchemaField {
                    name,
                    type_name: field_type_name(f.get("type")),
                })
            })
            .collect(),
    })
}

fn field_type_name(ty: Option<&Value>) -> String {
    if let Some(name) = named_type(ty) {
        return name.to_string();
    }
    if type_kind(ty) == Some("NON_NULL") {
        if let Some(inner) = named_type(ty.and_then(|ty| ty.get("ofType"))) {
            return format!("{inner}!");
        }
    }
    type_kind(ty).unwrap_or("unknown").to_string()
}

fn named_type(ty: Option<&Value>) -> Option<&str> {
    ty?.get("name")?.as_str()
}

fn type_kind(ty: Option<&Value>) -> Option<&str> {
    ty?.get("kind")?.as_str()
}

pub(crate) fn parameters(schema: &str) -> Result<Value> {
    let base = schema.trim_end_matches('!');
    let mut shape = if let Some(item) = base.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        json!({"type":"array", "items":parameters(item)?})
    } else {
        match base {
            "String" | "ID" | "DateTime" | "Blob" => json!({"type":"string"}),
            "Int" => json!({"type":"integer"}),
            "Float32" | "Float64" | "Float" => json!({"type":"number"}),
            "Boolean" => json!({"type":"boolean"}),
            "JSON" => json!({"type":["object", "array", "string", "number", "boolean"]}),
            _ => bail!("unsupported bounded write field type `{schema}`"),
        }
    };
    if !schema.ends_with('!') {
        if let Some(kind) = shape.get("type").cloned() {
            let mut kinds = match kind {
                Value::Array(kinds) => kinds,
                kind => vec![kind],
            };
            kinds.push(json!("null"));
            shape["type"] = Value::Array(kinds);
        }
    }
    Ok(shape)
}

/// Lean `CompletionRetry.CountCarrier.canCarry`: DateTime and ID take text
/// input but store no count (an ID accepts only a document ID).
pub fn can_hold_canonical_count(schema: &str) -> bool {
    if matches!(schema.trim_end_matches('!'), "DateTime" | "ID") {
        return false;
    }
    parameters(schema).is_ok_and(|shape| match shape.get("type") {
        Some(Value::Array(kinds)) => kinds.iter().any(admits_count),
        Some(kind) => admits_count(kind),
        None => false,
    })
}

fn admits_count(kind: &Value) -> bool {
    matches!(kind.as_str(), Some("integer" | "number" | "string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn introspection_query_rejects_injection() {
        let err = introspection_query("X\") { } evil").unwrap_err();
        assert!(err.to_string().contains("invalid"), "{err}");
    }
}
