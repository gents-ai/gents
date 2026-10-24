//! Raw GraphQL fragments require validation; string escaping and identifier validation do not cover them.

use defra_node::{EmbeddedNode, QueryResponse};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use crate::config_client::ConfigClientError;

pub use gents_protocol::graphql::{
    escape_graphql_string, validate_collection_identifier, validate_graphql_filter_fragment,
    validate_graphql_name,
};

#[derive(Debug, thiserror::Error)]
pub enum GraphqlResponseError {
    /// Error list returned by DefraDB for `operation`.
    #[error("{operation} failed: {errors:?}")]
    OperationFailed {
        operation: String,
        errors: Vec<query::QueryResponseError>,
    },
    #[error("decode {field} rows")]
    DecodeRows {
        field: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("decode {field} rows")]
    RowsNotArray { field: String },
    #[error(
        "mutation response returned both {field} and {normalized}; expected one normalized field"
    )]
    AmbiguousMutationField { field: String, normalized: String },
    #[error("{field} returned an unexpected mutation response shape")]
    MutationShape { field: String },
    #[error("{field} returned GraphQL errors")]
    MutationErrors { field: String },
    #[error("{field} omitted response data")]
    MissingData { field: String },
    #[error("{field} returned {count} documents; expected at most one")]
    TooManyDocuments { field: String, count: usize },
    #[error("create {collection} omitted its physical identity")]
    MissingCreatedIdentity { collection: String },
    #[error("decode composite commit rows")]
    DecodeCompositeCommits(#[source] serde_json::Error),
    #[error("{operation} returned no _version array")]
    MissingVersionArray { operation: String },
    #[error(
        "{operation} returned multiple composite commits at newest height {height}; document version is ambiguous"
    )]
    AmbiguousVersion { operation: String, height: i64 },
    #[error(transparent)]
    Access(Box<ConfigClientError>),
}

impl From<ConfigClientError> for GraphqlResponseError {
    fn from(error: ConfigClientError) -> Self {
        Self::Access(Box::new(error))
    }
}

type Result<T, E = GraphqlResponseError> = std::result::Result<T, E>;

/// Render a GraphQL string-list literal, emitting `null` for an empty list
/// (never `[]`, which types as `JsonArray` and corrupts nillable array columns).
pub fn graphql_string_list_literal<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let items = values
        .into_iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>();
    if items.is_empty() {
        "null".to_string()
    } else {
        format!("[{}]", items.join(", "))
    }
}

/// Read `collection`'s arrival head so the transaction conflicts with any
/// concurrent insert into it; predicate reads alone do not fence inserts.
pub async fn fence_arrivals(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    collection: &str,
) -> Result<(), ConfigClientError> {
    txn.execute(&format!(
        r#"{{ _documentArrivals(collection: "{}", limit: 1) {{ head }} }}"#,
        escape_graphql_string(collection)
    ))
    .await?;
    Ok(())
}

pub async fn graphql_response_with_transaction_retry(
    node: &EmbeddedNode,
    graphql: &str,
    operation: &str,
) -> Result<QueryResponse, ConfigClientError> {
    crate::config_client::ConfigAccess::read_local_response(node, graphql, operation).await
}

pub async fn graphql_with_transaction_retry(
    node: &EmbeddedNode,
    graphql: &str,
    operation: &str,
) -> Result<QueryResponse, ConfigClientError> {
    let response = graphql_response_with_transaction_retry(node, graphql, operation).await?;
    ensure_no_errors(&response, operation)?;
    Ok(response)
}

pub(crate) fn mutation_affected_documents(response: &QueryResponse) -> usize {
    response
        .data
        .as_ref()
        .and_then(Value::as_object)
        .map(|fields| {
            fields
                .values()
                .map(|value| match value {
                    Value::Array(rows) => rows.len(),
                    Value::Object(_) => 1,
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or_default()
}

pub(crate) fn into_data_envelope(data: Option<Value>) -> Value {
    Value::Object(serde_json::Map::from_iter([(
        "data".to_owned(),
        data.unwrap_or(Value::Null),
    )]))
}

pub fn ensure_no_errors(response: &QueryResponse, operation: &str) -> Result<()> {
    if response.has_errors() {
        return Err(GraphqlResponseError::OperationFailed {
            operation: operation.to_owned(),
            errors: response.errors.clone(),
        });
    }
    Ok(())
}

pub fn canonical_positive_count(value: &Value, maximum: usize) -> Option<usize> {
    let parsed = match value {
        Value::Number(number) => number
            .as_u64()
            .and_then(|value| usize::try_from(value).ok()),
        Value::String(value)
            if !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && (value == "0" || !value.starts_with('0')) =>
        {
            value.parse::<usize>().ok()
        }
        _ => None,
    }?;
    (1..=maximum).contains(&parsed).then_some(parsed)
}

pub fn rows<T>(response: &QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let Some(value) = response.data.as_ref().and_then(|data| data.get(field)) else {
        return Ok(Vec::new());
    };
    Vec::<T>::deserialize(value).map_err(|source| GraphqlResponseError::DecodeRows {
        field: field.to_owned(),
        source,
    })
}

pub fn first_row<T>(response: &QueryResponse, field: &str) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    Ok(rows(response, field)?.into_iter().next())
}

fn mutation_documents_in_data<'a>(
    data: Option<&'a Value>,
    field: &str,
) -> Result<Option<&'a [Value]>> {
    let Some(data) = data.and_then(Value::as_object) else {
        return Ok(None);
    };
    let normalized_field = field
        .strip_prefix("create_")
        .map(|collection| format!("add_{collection}"))
        .or_else(|| {
            field
                .strip_prefix("add_")
                .map(|collection| format!("create_{collection}"))
        });
    let direct = data.get(field);
    let normalized = normalized_field
        .as_deref()
        .and_then(|field| data.get(field));
    let value = match (direct, normalized) {
        (Some(_), Some(_)) => {
            return Err(GraphqlResponseError::AmbiguousMutationField {
                field: field.to_owned(),
                normalized: normalized_field.expect("normalized field exists"),
            })
        }
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => return Ok(None),
    };
    match value {
        Value::Object(_) => Ok(Some(std::slice::from_ref(value))),
        Value::Array(rows) => Ok(Some(rows.as_slice())),
        _ => Err(GraphqlResponseError::MutationShape {
            field: field.to_owned(),
        }),
    }
}

/// Return borrowed documents from a create/update mutation envelope.
///
/// DefraDB has returned both object and array mutation shapes, and may expose
/// a `create_` request under its normalized `add_` response field. Centralize
/// that compatibility handling so callers only interpret returned documents.
pub fn mutation_documents<'a>(response: &'a Value, field: &str) -> Result<Option<&'a [Value]>> {
    if !response
        .get("errors")
        .is_none_or(|errors| errors.is_null() || errors.as_array().is_some_and(Vec::is_empty))
    {
        return Err(GraphqlResponseError::MutationErrors {
            field: field.to_owned(),
        });
    }
    let data = response
        .get("data")
        .ok_or_else(|| GraphqlResponseError::MissingData {
            field: field.to_owned(),
        })?;
    mutation_documents_in_data(Some(data), field)
}

/// Return the single document selected by a create/update mutation.
/// More than one row is an error because provenance-bearing mutations are
/// single-document writes.
pub fn single_mutation_document<'a>(
    response: &'a QueryResponse,
    field: &str,
) -> Result<Option<&'a Value>> {
    single_mutation_document_in_data(response.data.as_ref(), field)
}

fn single_mutation_document_in_data<'a>(
    data: Option<&'a Value>,
    field: &str,
) -> Result<Option<&'a Value>> {
    let Some(documents) = mutation_documents_in_data(data, field)? else {
        return Ok(None);
    };
    match documents.len() {
        0 => Ok(None),
        1 => Ok(documents.first()),
        count => Err(GraphqlResponseError::TooManyDocuments {
            field: field.to_owned(),
            count,
        }),
    }
}

pub fn created_doc_id(response: &Value, collection: &str) -> Result<String> {
    let field = format!("create_{collection}");
    let documents = mutation_documents(response, &field)?;
    if let Some(documents) = documents.as_ref() {
        if documents.len() > 1 {
            return Err(GraphqlResponseError::TooManyDocuments {
                field,
                count: documents.len(),
            });
        }
    }
    documents
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| GraphqlResponseError::MissingCreatedIdentity {
            collection: collection.to_owned(),
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CompositeCommit {
    pub cid: String,
    pub height: i64,
    #[serde(rename = "fieldName")]
    pub field_name: String,
    #[serde(default)]
    pub signature: Option<CommitSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CommitSignature {
    #[serde(default)]
    pub identity: String,
    #[serde(rename = "type", default)]
    pub signature_type: String,
}

/// Decode the selected `_commits` rows and retain only DefraDB's composite
/// document versions. `_commits` filters are evaluated in memory and can
/// silently degrade to no filter, so every reader applies this selection here.
pub(crate) fn decode_composite_commits(rows: &[Value]) -> Result<Vec<CompositeCommit>> {
    let mut commits = rows
        .iter()
        .cloned()
        .map(serde_json::from_value::<CompositeCommit>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(GraphqlResponseError::DecodeCompositeCommits)?
        .into_iter()
        .filter(|commit| commit.field_name == "_C")
        .collect::<Vec<_>>();
    commits.sort_by(|left, right| {
        right
            .height
            .cmp(&left.height)
            .then_with(|| left.cid.cmp(&right.cid))
    });
    Ok(commits)
}

fn response_composite_commits(
    response: &QueryResponse,
    field: &str,
) -> Result<Vec<CompositeCommit>> {
    composite_commits_at(
        response.data.as_ref().and_then(|data| data.get(field)),
        field,
    )
}

fn composite_commits_at(value: Option<&Value>, field: &str) -> Result<Vec<CompositeCommit>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let rows = value
        .as_array()
        .ok_or_else(|| GraphqlResponseError::RowsNotArray {
            field: field.to_owned(),
        })?;
    decode_composite_commits(rows)
}

pub fn mutation_composite_version(
    response: &QueryResponse,
    field: &str,
) -> Result<Option<CompositeCommit>> {
    let Some(document) = single_mutation_document(response, field)? else {
        return Ok(None);
    };
    document_composite_version(document, field)
}

/// Select the unique newest composite commit from a document `_version`
/// projection. Field commits cannot identify an exact document version and
/// are therefore never accepted as a fallback.
pub fn document_composite_version(
    document: &Value,
    operation: &str,
) -> Result<Option<CompositeCommit>> {
    let versions = document
        .get("_version")
        .and_then(Value::as_array)
        .ok_or_else(|| GraphqlResponseError::MissingVersionArray {
            operation: operation.to_owned(),
        })?;
    let commits = decode_composite_commits(versions)?;
    unique_newest_composite_commit(commits, operation)
}

fn unique_newest_composite_commit(
    mut commits: Vec<CompositeCommit>,
    operation: &str,
) -> Result<Option<CompositeCommit>> {
    commits.sort_by(|left, right| {
        right
            .height
            .cmp(&left.height)
            .then_with(|| left.cid.cmp(&right.cid))
    });
    let Some(newest) = commits.first() else {
        return Ok(None);
    };
    if commits
        .get(1)
        .is_some_and(|candidate| candidate.height == newest.height)
    {
        return Err(GraphqlResponseError::AmbiguousVersion {
            operation: operation.to_owned(),
            height: newest.height,
        });
    }
    Ok(commits.into_iter().next())
}

pub async fn newest_document_composite_commit(
    node: &EmbeddedNode,
    doc_id: &str,
    operation: &str,
) -> Result<Option<CompositeCommit>> {
    let query = newest_composite_query(doc_id);
    let response = graphql_with_transaction_retry(node, &query, operation).await?;
    let commits = response_composite_commits(&response, "_commits")?;
    unique_newest_composite_commit(commits, operation)
}

pub async fn newest_document_composite_commit_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    doc_id: &str,
    operation: &str,
) -> Result<Option<CompositeCommit>> {
    let response = txn.execute(&newest_composite_query(doc_id)).await?;
    let commits = composite_commits_at(
        response.get("data").and_then(|data| data.get("_commits")),
        "_commits",
    )?;
    unique_newest_composite_commit(commits, operation)
}

fn newest_composite_query(doc_id: &str) -> String {
    format!(
        r#"query {{
            _commits(
                docID: "{}"
                depth: 1
                filter: {{ fieldName: {{ _eq: "_C" }} }}
                order: {{ height: DESC }}
                limit: 2
            ) {{ cid height fieldName }}
        }}"#,
        escape_graphql_string(doc_id),
    )
}

/// Load a document's DefraDB-native composite versions, newest first.
///
/// The returned CID is the time-travel reference; signature fields are the
/// stored commit evidence and must not be confused with application DID
/// columns on the document.
pub async fn composite_commits(
    node: &EmbeddedNode,
    doc_id: &str,
    operation: &str,
) -> Result<Vec<CompositeCommit>> {
    let query = format!(
        r#"query {{
            _commits(docID: "{}") {{
                cid
                height
                fieldName
                signature {{ identity type }}
            }}
        }}"#,
        escape_graphql_string(doc_id),
    );
    let response = graphql_with_transaction_retry(node, &query, operation).await?;
    response_composite_commits(&response, "_commits")
}

pub fn response_has_documents(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::Object(map) => map.contains_key("_docID"),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
