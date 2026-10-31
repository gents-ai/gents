use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

use super::query::{collect_filter_field_keys, is_restricted_field, DefraQueryParams};
use super::render::validate_identifier;
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

    pub(crate) fn visible_fields(&self, collection: &str) -> Vec<&SchemaField> {
        self.fields
            .iter()
            .filter(|f| !is_restricted_field(collection, &f.name))
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

pub fn unknown_collection_message(collection: &str) -> String {
    format!(
        "collection {collection:?} does not exist; {}",
        super::command::COLLECTION_GUIDANCE
    )
}

pub(crate) fn suggest_fields(invalid: &str, candidates: &[String]) -> Vec<String> {
    let threshold = (invalid.chars().count() / 2).max(2);
    let mut scored: Vec<(usize, &String)> = candidates
        .iter()
        .map(|c| (levenshtein(invalid, c), c))
        .filter(|(distance, _)| *distance > 0 && *distance <= threshold)
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(3).map(|(_, c)| c.clone()).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous_diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous_diagonal + usize::from(ca != cb);
            previous_diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(previous_diagonal + 1);
        }
    }
    row[b.len()]
}

pub fn diagnose_failed_query(
    params: &DefraQueryParams,
    schema: Option<&CollectionSchema>,
    raw_errors: &str,
) -> String {
    let Some(schema) = schema else {
        return unknown_collection_message(&params.collection);
    };
    let known = schema.field_names();
    let visible: Vec<String> = schema
        .visible_fields(&params.collection)
        .iter()
        .map(|f| f.name.clone())
        .collect();

    let mut requested: Vec<String> = params.fields.clone();
    if let Some(filter) = params.filter.as_ref() {
        collect_filter_field_keys(filter, &mut requested);
    }
    let mut invalid: Vec<&str> = Vec::new();
    for field in &requested {
        if !known.contains(field.as_str()) && !invalid.contains(&field.as_str()) {
            invalid.push(field);
        }
    }
    if invalid.is_empty() {
        return raw_errors.to_string();
    }

    let clauses: Vec<String> = invalid
        .iter()
        .map(|field| {
            let suggestions = suggest_fields(field, &visible);
            if suggestions.is_empty() {
                format!("unknown field {field:?}")
            } else {
                let alternatives: Vec<String> =
                    suggestions.iter().map(|s| format!("{s:?}")).collect();
                format!(
                    "unknown field {field:?} (did you mean {}?)",
                    alternatives.join(" or ")
                )
            }
        })
        .collect();
    format!(
        "{clauses} on collection {collection:?}; queryable fields: [{fields}]. \
         Tip: call query with argv: [\"fields\"] and collection to list its fields",
        clauses = clauses.join("; "),
        collection = params.collection,
        fields = visible.join(", "),
    )
}

pub fn discovery_payload(collection: &str, schema: &CollectionSchema) -> Value {
    let fields: Vec<Value> = schema
        .visible_fields(collection)
        .iter()
        .map(|f| json!({ "name": f.name, "type": f.type_name }))
        .collect();
    json!({
        "collection": collection,
        "discovery": true,
        "count": fields.len(),
        "fields": fields,
        "note": "Field inventory (restricted fields excluded). Call defra_query again with specific field names.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_from(names_and_types: &[(&str, &str)]) -> CollectionSchema {
        CollectionSchema {
            fields: names_and_types
                .iter()
                .map(|(n, t)| SchemaField {
                    name: n.to_string(),
                    type_name: t.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn introspection_query_rejects_injection() {
        let err = introspection_query("X\") { } evil").unwrap_err();
        assert!(err.to_string().contains("invalid"), "{err}");
    }

    #[test]
    fn suggestions_never_include_restricted_fields() {
        let schema = schema_from(&[("backend_id", "String"), ("auth", "JSON")]);
        let candidates: Vec<String> = schema
            .visible_fields("InferenceBackend")
            .iter()
            .map(|f| f.name.clone())
            .collect();
        assert!(suggest_fields("auths", &candidates).is_empty());
    }
}
