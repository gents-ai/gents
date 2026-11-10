use serde_json::Value;

use super::{GraphqlError, Result};

pub fn extract_mutation_doc_id(
    response: &serde_json::Value,
    collection_name: &str,
) -> Result<String> {
    let data = response
        .get("data")
        .ok_or_else(|| GraphqlError::MissingData {
            response: response.to_string(),
        })?;
    for field_name in [
        format!("upsert_{collection_name}"),
        format!("update_{collection_name}"),
        format!("create_{collection_name}"),
        format!("add_{collection_name}"),
    ] {
        if let Some(doc_id) = data
            .get(&field_name)
            .and_then(|value| value.get("_docID"))
            .and_then(serde_json::Value::as_str)
        {
            return Ok(doc_id.to_string());
        }
        if let Some(doc_id) = data
            .get(&field_name)
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(serde_json::Value::as_str)
        {
            return Ok(doc_id.to_string());
        }
    }
    Err(GraphqlError::MutationDocId {
        collection: collection_name.to_owned(),
        response: response.to_string(),
    })
}

pub fn first_graphql_row<'a>(
    response: &'a serde_json::Value,
    collection_name: &str,
) -> Result<&'a serde_json::Value> {
    response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| GraphqlError::NoRows {
            collection: collection_name.to_owned(),
        })
}

pub fn graphql_rows_from_response(response: &Value, collection_name: &str) -> Vec<Value> {
    response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// External premise: DefraDB reports an unknown collection only in its
/// GraphQL error message.
///
/// Returns true only when GraphQL reports that the collection's root field is
/// absent. A missing column on an existing collection must remain an error;
/// treating every `Cannot query field` mentioning the collection type as a
/// missing collection hides schema/query drift.
pub fn is_collection_missing_error_message(collection_name: &str, message: &str) -> bool {
    let missing_root_double_quoted = format!(r#"Cannot query field \"{collection_name}\""#);
    let missing_root_single_quoted = format!("Cannot query field '{collection_name}'");
    (message.contains(collection_name) && message.contains("collection not found"))
        || message.contains(&missing_root_double_quoted)
        || message.contains(&missing_root_single_quoted)
}
