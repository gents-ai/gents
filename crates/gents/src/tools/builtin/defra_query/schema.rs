use serde_json::{json, Value};

use super::query::{collect_filter_field_keys, is_restricted_field, DefraQueryParams};
use crate::store::config_client::schema_introspection::{CollectionSchema, SchemaField};

pub(crate) fn visible_fields<'a>(
    schema: &'a CollectionSchema,
    collection: &str,
) -> Vec<&'a SchemaField> {
    schema.visible_fields(|name| is_restricted_field(collection, name))
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
    let visible: Vec<String> = visible_fields(schema, &params.collection)
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
    let fields: Vec<Value> = visible_fields(schema, collection)
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
    fn suggestions_never_include_restricted_fields() {
        let schema = schema_from(&[("backend_id", "String"), ("auth", "JSON")]);
        let candidates: Vec<String> = visible_fields(&schema, "InferenceBackend")
            .iter()
            .map(|f| f.name.clone())
            .collect();
        assert!(suggest_fields("auths", &candidates).is_empty());
    }
}
