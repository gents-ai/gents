//! Collection-field discovery and invalid-field diagnostics for `defra_query`.
//!
//! When a query fails (or the caller asks for `fields: ["*"]`), the collection
//! is introspected via GraphQL `__type` and the schema field set is turned into
//! agent-usable output: the allowed field inventory (restricted sensitive
//! fields excluded) and, for invalid fields, close-match suggestions. This
//! keeps the diagnostics structural — no hard-coded per-collection field lists.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

use super::query::{collect_filter_field_keys, is_restricted_field, DefraQueryParams};
use super::render::validate_identifier;
use crate::graphql::escape_graphql_string;

/// A field on a collection as reported by GraphQL introspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaField {
    pub name: String,
    /// Best-effort type display: the named type when present (e.g. `String`,
    /// `DateTime`), otherwise the type kind (e.g. `LIST`).
    pub type_name: String,
}

/// The introspected field set of one collection.
#[derive(Debug, Clone)]
pub struct CollectionSchema {
    pub fields: Vec<SchemaField>,
}

impl CollectionSchema {
    /// Every introspected field name, including DefraDB internals and
    /// aggregate pseudo-fields. This is the *validity* set: anything outside
    /// it is definitively not queryable.
    pub(crate) fn field_names(&self) -> BTreeSet<&str> {
        self.fields.iter().map(|f| f.name.as_str()).collect()
    }

    /// The *display* set: fields worth advertising to an agent. Excludes
    /// restricted sensitive fields, aggregate pseudo-fields (`AVG`, `COUNT`,
    /// ...), and `_`-prefixed internals except `_docID`.
    pub(crate) fn visible_fields(&self, collection: &str) -> Vec<&SchemaField> {
        self.fields
            .iter()
            .filter(|f| !is_restricted_field(collection, &f.name))
            .filter(|f| !f.name.chars().all(|c| c.is_ascii_uppercase()))
            .filter(|f| !f.name.starts_with('_') || f.name == "_docID")
            .collect()
    }
}

/// Build the `__type` introspection query for a collection. The name is
/// validated as an identifier and escaped, so untrusted input cannot inject.
pub fn introspection_query(collection: &str) -> Result<String> {
    validate_identifier(collection).map_err(|e| anyhow!("invalid collection name: {e}"))?;
    Ok(format!(
        r#"{{ __type(name: "{name}") {{ fields {{ name type {{ name kind }} }} }} }}"#,
        name = escape_graphql_string(collection)
    ))
}

/// Parse the `data` of an introspection response into a [`CollectionSchema`].
/// Returns `None` when the type does not exist (`__type` is `null`).
pub fn parse_collection_schema(data: Option<&Value>) -> Option<CollectionSchema> {
    let fields = data?.get("__type")?.get("fields")?.as_array()?;
    Some(CollectionSchema {
        fields: fields
            .iter()
            .filter_map(|f| {
                let name = f.get("name")?.as_str()?.to_string();
                let ty = f.get("type");
                let type_name = ty
                    .and_then(|t| t.get("name"))
                    .and_then(Value::as_str)
                    .or_else(|| ty.and_then(|t| t.get("kind")).and_then(Value::as_str))
                    .unwrap_or("unknown")
                    .to_string();
                Some(SchemaField { name, type_name })
            })
            .collect(),
    })
}

/// The shared "no such collection" message, used by both the failure
/// diagnostic and discovery mode.
pub fn unknown_collection_message(collection: &str) -> String {
    format!(
        "collection {collection:?} does not exist; check the collection (GraphQL type) name, e.g. \"AgentRequest\""
    )
}

/// Suggest close matches for an invalid field name from the candidate set,
/// nearest first, at most three, only when reasonably close (edit distance at
/// most half the invalid name's length).
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

/// Build the enriched failure message for a query that DefraDB rejected.
///
/// With a schema in hand, requested selection fields and filter field keys are
/// checked against the introspected field set; any misses produce a diagnostic
/// with the allowed inventory and suggestions. When the collection itself does
/// not exist (`schema` is `None`) that is said plainly. When nothing looks
/// field-shaped (all names valid), falls back to `raw_errors`.
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
         Tip: call defra_query with fields: [\"*\"] to list a collection's fields",
        clauses = clauses.join("; "),
        collection = params.collection,
        fields = visible.join(", "),
    )
}

/// The success payload for discovery mode (`fields: ["*"]`): the queryable
/// field inventory for the collection, restricted fields excluded.
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
    use serde_json::json;

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

    /// The introspected AgentToolCall-ish fixture used across tests: internals,
    /// aggregates, and real fields, matching what DefraDB actually returns.
    fn tool_call_schema() -> CollectionSchema {
        schema_from(&[
            ("AVG", "Float"),
            ("COUNT", "Int"),
            ("GROUP", "LIST"),
            ("_deleted", "Boolean"),
            ("_docID", "ID"),
            ("_version", "LIST"),
            ("tool_call_key", "String"),
            ("tool_name", "String"),
            ("status", "String"),
            ("started_at", "DateTime"),
            ("completed_at", "DateTime"),
            ("deadline_at", "DateTime"),
            ("request_id", "String"),
        ])
    }

    fn params(collection: &str, fields: &[&str]) -> DefraQueryParams {
        DefraQueryParams {
            collection: collection.to_string(),
            filter: None,
            fields: fields.iter().map(|f| f.to_string()).collect(),
            limit: None,
        }
    }

    #[test]
    fn introspection_query_targets_the_collection() {
        let q = introspection_query("AgentToolCall").unwrap();
        assert!(q.contains(r#"__type(name: "AgentToolCall")"#), "{q}");
        assert!(q.contains("fields"), "{q}");
    }

    #[test]
    fn introspection_query_rejects_injection() {
        let err = introspection_query("X\") { } evil").unwrap_err();
        assert!(err.to_string().contains("invalid"), "{err}");
    }

    #[test]
    fn parse_extracts_field_names_and_types() {
        let data = json!({
            "__type": {
                "fields": [
                    { "name": "status", "type": { "kind": "SCALAR", "name": "String" } },
                    { "name": "started_at", "type": { "kind": "SCALAR", "name": "DateTime" } },
                    { "name": "_version", "type": { "kind": "LIST", "name": null } }
                ]
            }
        });
        let schema = parse_collection_schema(Some(&data)).expect("type exists");
        assert_eq!(
            schema.fields,
            vec![
                SchemaField {
                    name: "status".into(),
                    type_name: "String".into()
                },
                SchemaField {
                    name: "started_at".into(),
                    type_name: "DateTime".into()
                },
                SchemaField {
                    name: "_version".into(),
                    type_name: "LIST".into()
                },
            ]
        );
    }

    #[test]
    fn parse_returns_none_for_unknown_type() {
        let data = json!({ "__type": null });
        assert!(parse_collection_schema(Some(&data)).is_none());
        assert!(parse_collection_schema(None).is_none());
    }

    #[test]
    fn visible_fields_hide_aggregates_and_internals_but_keep_docid() {
        let schema = tool_call_schema();
        let visible: Vec<&str> = schema
            .visible_fields("AgentToolCall")
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(visible.contains(&"_docID"), "{visible:?}");
        assert!(visible.contains(&"tool_name"), "{visible:?}");
        assert!(visible.contains(&"started_at"), "{visible:?}");
        for hidden in ["AVG", "COUNT", "GROUP", "_version", "_deleted"] {
            assert!(!visible.contains(&hidden), "{hidden} should be hidden");
        }
    }

    #[test]
    fn visible_fields_exclude_restricted_secrets() {
        let schema = schema_from(&[
            ("backend_id", "String"),
            ("endpoint", "String"),
            ("api_key", "String"),
            ("api_key_env_var", "String"),
        ]);
        let visible: Vec<&str> = schema
            .visible_fields("InferenceBackend")
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(visible, vec!["backend_id", "endpoint"]);
    }

    #[test]
    fn suggests_timestamp_neighbours_for_created_at() {
        let candidates: Vec<String> = tool_call_schema()
            .visible_fields("AgentToolCall")
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let suggestions = suggest_fields("created_at", &candidates);
        assert!(
            suggestions.contains(&"started_at".to_string()),
            "{suggestions:?}"
        );
        assert!(
            suggestions.contains(&"completed_at".to_string()),
            "{suggestions:?}"
        );
        assert!(
            !suggestions.contains(&"deadline_at".to_string()),
            "deadline_at is too far: {suggestions:?}"
        );
    }

    #[test]
    fn suggests_agent_did_for_agent_name() {
        let candidates = vec![
            "agent_did".to_string(),
            "behavior_id".to_string(),
            "request_id".to_string(),
            "status".to_string(),
        ];
        assert_eq!(suggest_fields("agent_name", &candidates), vec!["agent_did"]);
    }

    #[test]
    fn no_suggestions_for_nonsense() {
        let candidates = vec!["request_id".to_string(), "status".to_string()];
        assert!(suggest_fields("zzzqqqxxx", &candidates).is_empty());
    }

    #[test]
    fn suggestions_never_include_restricted_fields() {
        // Candidates come from the visible set, so a near-miss on a secret
        // field must not resurrect it.
        let schema = schema_from(&[
            ("backend_id", "String"),
            ("api_key", "String"),
            ("api_key_env_var", "String"),
        ]);
        let candidates: Vec<String> = schema
            .visible_fields("InferenceBackend")
            .iter()
            .map(|f| f.name.clone())
            .collect();
        assert!(suggest_fields("api_keys", &candidates).is_empty());
    }

    #[test]
    fn diagnose_reports_invalid_selection_fields_with_inventory_and_suggestions() {
        let schema = tool_call_schema();
        let msg = diagnose_failed_query(
            &params("AgentToolCall", &["tool_name", "created_at"]),
            Some(&schema),
            "raw",
        );
        assert!(msg.contains("created_at"), "{msg}");
        assert!(msg.contains("AgentToolCall"), "{msg}");
        assert!(msg.contains("started_at"), "{msg}");
        assert!(msg.contains("completed_at"), "{msg}");
        // Inventory present: a valid field the caller did NOT request.
        assert!(msg.contains("tool_call_key"), "{msg}");
        // Aggregates are not advertised.
        assert!(!msg.contains("AVG"), "{msg}");
    }

    #[test]
    fn diagnose_reports_invalid_filter_keys() {
        let schema = tool_call_schema();
        let mut p = params("AgentToolCall", &["tool_name"]);
        p.filter = Some(json!({ "_and": [{ "created_at": { "_gt": "2026" } }] }));
        let msg = diagnose_failed_query(&p, Some(&schema), "raw");
        assert!(msg.contains("created_at"), "{msg}");
        assert!(msg.contains("started_at"), "{msg}");
    }

    #[test]
    fn diagnose_reports_unknown_collection() {
        let msg = diagnose_failed_query(&params("NoSuchThing", &["x"]), None, "raw");
        assert!(msg.contains("NoSuchThing"), "{msg}");
        assert!(msg.contains("does not exist"), "{msg}");
    }

    #[test]
    fn diagnose_falls_back_to_raw_errors_when_fields_all_valid() {
        let schema = tool_call_schema();
        let msg = diagnose_failed_query(
            &params("AgentToolCall", &["tool_name"]),
            Some(&schema),
            "some backend explosion",
        );
        assert!(msg.contains("some backend explosion"), "{msg}");
    }

    #[test]
    fn discovery_payload_lists_visible_fields_with_types() {
        let schema = tool_call_schema();
        let payload = discovery_payload("AgentToolCall", &schema);
        assert_eq!(payload["collection"], "AgentToolCall");
        assert_eq!(payload["discovery"], true);
        let fields = payload["fields"].as_array().expect("fields array");
        let names: Vec<&str> = fields.iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"started_at"), "{names:?}");
        assert!(!names.contains(&"AVG"), "{names:?}");
        let started = fields
            .iter()
            .find(|f| f["name"] == "started_at")
            .expect("started_at present");
        assert_eq!(started["type"], "DateTime");
    }
}
