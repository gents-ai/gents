//! Shared GraphQL utility functions used across Gents runtime modules.
//!
//! Two defenses apply, one per grammatical position:
//! - [`escape_graphql_string`] for values interpolated inside **string
//!   literals** — escaping makes any content safe to embed.
//! - [`validate_graphql_name`] / [`validate_collection_identifier`] for
//!   values interpolated as bare **identifiers** (collection names, field
//!   names, mutation input keys). Identifiers sit outside string literals,
//!   so escaping cannot apply; validation against the GraphQL Name grammar
//!   is the only defense.
//!
//! The identifier validators live in `gents-protocol` so the mutation
//! renderer there shares this crate's definition, and are re-exported here.
//!
//! A third position — a raw **object/fragment** spliced in whole, as
//! `EventTrigger.filter` is by the trigger engine's filter probe — is
//! covered by neither. See #1038.

use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryResponse};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

pub use gents_protocol::graphql::{
    validate_collection_identifier, validate_graphql_filter_fragment, validate_graphql_name,
};

pub const DEFRA_DB_CONFLICT_MAX_RETRIES: u32 = 3;
pub const DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS: u64 = 100;

const GRAPHQL_RETRY_POLICY: ExecuteRetryPolicy = ExecuteRetryPolicy::new(
    DEFRA_DB_CONFLICT_MAX_RETRIES,
    Duration::from_millis(DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS),
    Duration::from_millis(800),
);

/// Execute GraphQL through the node's identity-aware retry path.
///
/// This is the low-level form for callers that intentionally inspect GraphQL
/// errors. Most callers should use [`graphql_with_transaction_retry`].
pub async fn graphql_response_with_transaction_retry(
    node: &EmbeddedNode,
    graphql: &str,
    operation: &str,
) -> QueryResponse {
    let started = std::time::Instant::now();
    let response = node.execute_with_retry(graphql, GRAPHQL_RETRY_POLICY).await;
    let elapsed = started.elapsed();
    if elapsed > Duration::from_secs(1) {
        tracing::warn!(
            operation,
            elapsed_ms = elapsed.as_millis() as u64,
            "DefraDB GraphQL completed"
        );
    } else {
        tracing::debug!(
            operation,
            elapsed_ms = elapsed.as_millis() as u64,
            "DefraDB GraphQL completed"
        );
    }
    response
}

/// Execute identity-aware GraphQL with transaction-conflict retry and fail on
/// GraphQL errors.
pub async fn graphql_with_transaction_retry(
    node: &EmbeddedNode,
    graphql: &str,
    operation: &str,
) -> Result<QueryResponse> {
    let response = graphql_response_with_transaction_retry(node, graphql, operation).await;
    ensure_no_errors(&response, operation)?;
    Ok(response)
}

pub fn ensure_no_errors(response: &QueryResponse, operation: &str) -> Result<()> {
    if response.has_errors() {
        anyhow::bail!("{operation} failed: {:?}", response.errors);
    }
    Ok(())
}

pub fn rows<T>(response: &QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let Some(value) = response.data.as_ref().and_then(|data| data.get(field)) else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).with_context(|| format!("decode {field} rows"))
}

pub fn first_row<T>(response: &QueryResponse, field: &str) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    Ok(rows(response, field)?.into_iter().next())
}

/// Return the single document selected by a create/update mutation.
///
/// DefraDB has returned both an object and a one-element array for mutation
/// fields across API generations. Normalizing that shape here keeps callers
/// from growing subtly different response parsers. More than one row is an
/// error because none of the provenance-bearing mutations are bulk writes.
pub fn single_mutation_document<'a>(
    response: &'a QueryResponse,
    field: &str,
) -> Result<Option<&'a Value>> {
    let Some(data) = response.data.as_ref().and_then(Value::as_object) else {
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
        (Some(_), Some(_)) => anyhow::bail!(
            "mutation response returned both {field} and {}; expected one normalized field",
            normalized_field.expect("normalized field exists")
        ),
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => return Ok(None),
    };
    match value {
        Value::Object(_) => Ok(Some(value)),
        Value::Array(rows) => match rows.as_slice() {
            [] => Ok(None),
            [row] => Ok(Some(row)),
            _ => anyhow::bail!(
                "{field} returned {} documents; expected at most one",
                rows.len()
            ),
        },
        _ => anyhow::bail!("{field} returned an unexpected mutation response shape"),
    }
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

/// Read the exact newest composite commit exposed by a mutation's `_version`
/// selection. DefraDB returns the full version history newest-first today; we
/// still sort explicitly so the contract does not depend on response order.
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
        .ok_or_else(|| anyhow::anyhow!("{operation} returned no _version array"))?;
    let commits = versions
        .iter()
        .cloned()
        .map(serde_json::from_value::<CompositeCommit>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|commit| commit.field_name == "_C")
        .collect::<Vec<_>>();
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
        anyhow::bail!(
            "{operation} returned multiple composite commits at newest height {}; document version is ambiguous",
            newest.height
        );
    }
    Ok(commits.into_iter().next())
}

/// Read at most the current composite heads for a document. `depth: 1` keeps
/// the query independent of document history length, while `limit: 2` retains
/// enough evidence to reject concurrent newest heads.
pub async fn newest_document_composite_commit(
    node: &EmbeddedNode,
    doc_id: &str,
    operation: &str,
) -> Result<Option<CompositeCommit>> {
    let query = format!(
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
    );
    let response = graphql_with_transaction_retry(node, &query, operation).await?;
    let commits = rows::<CompositeCommit>(&response, "_commits")?
        .into_iter()
        .filter(|commit| commit.field_name == "_C")
        .collect();
    unique_newest_composite_commit(commits, operation)
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
    // DefraDB applies this filter in memory and an invalid GraphQL filter can
    // degrade to no filter. Fetch the field name and enforce the composite
    // selection here instead of trusting that behavior.
    let mut commits = rows::<CompositeCommit>(&response, "_commits")?
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

pub fn is_defradb_transaction_conflict_text(text: &str) -> bool {
    text.to_ascii_lowercase().contains("transaction conflict")
}

pub fn defradb_conflict_retry_backoff(retry_index: u32) -> Duration {
    Duration::from_millis(
        DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS.saturating_mul(1u64 << retry_index.min(10)),
    )
}

pub fn escape_graphql_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
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
