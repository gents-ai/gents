//! Private HTTP plumbing for the committed-write owner.

use anyhow::{Context, Result};
use gents_protocol::graphql::{execute_graphql_async, GraphqlRequestOptions};
use serde_json::Value;

use super::graphql_api_base;
use super::retry;
use super::write_telemetry::ConflictSource;

const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("building DefraDB transaction HTTP client")
}

/// Submit an auto-commit mutation exactly once. DefraDB's HTTP handler owns
/// conflict retry inside this request; replay after an ambiguous transport
/// result could apply a mutation twice.
pub(super) async fn auto_commit(endpoint: &str, mutation: &str) -> Result<Value> {
    let response = http_client()?
        .post(endpoint)
        .json(&serde_json::json!({"query": mutation}))
        .send()
        .await
        .with_context(|| format!("posting GraphQL mutation to {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("reading GraphQL mutation response from {endpoint}"))?;
    let value: Value = response
        .json()
        .await
        .with_context(|| format!("decoding GraphQL mutation response from {endpoint}"))?;
    if graphql_response_has_errors(&value) {
        let errors = &value["errors"];
        anyhow::bail!("graphql returned errors: {errors}")
    }
    Ok(value)
}

pub(super) async fn query_with_options(
    endpoint: &str,
    query: &str,
    options: GraphqlRequestOptions,
) -> Result<Value> {
    ensure_query_document(query)?;
    execute_graphql_async(endpoint, query, options).await
}

pub(super) async fn txn_begin(endpoint: &str) -> Result<(reqwest::Client, String)> {
    let client = http_client()?;
    let response = client
        .post(format!("{}/tx", graphql_api_base(endpoint)?))
        .send()
        .await
        .with_context(|| format!("posting tx begin to {endpoint}"))?;
    let status = response.status();
    let bytes = response.bytes().await.context("reading tx begin body")?;
    if !status.is_success() {
        anyhow::bail!(
            "tx begin returned HTTP {status} from {endpoint}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    let body: Value = serde_json::from_slice(&bytes).context("decoding tx begin body")?;
    let id = body
        .get("id")
        .and_then(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| value.as_u64().map(|id| id.to_string()))
        })
        .ok_or_else(|| anyhow::anyhow!("tx begin missing id"))?;
    Ok((client, id))
}

/// Execute one statement in an existing transaction, with no statement-level
/// retry. A conflict invalidates the snapshot and must replay the whole owner
/// callback in a newly begun transaction.
pub(super) async fn txn_execute(
    endpoint: &str,
    id: &str,
    client: &reqwest::Client,
    query: &str,
    variables: &Value,
) -> Result<Value> {
    let response = client
        .post(endpoint)
        .header("x-defradb-tx", id)
        .json(&serde_json::json!({"query": query, "variables": variables}))
        .send()
        .await
        .with_context(|| format!("posting transactional GraphQL to {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("reading transactional GraphQL from {endpoint}"))?;
    let value: Value = response
        .json()
        .await
        .with_context(|| format!("decoding transactional GraphQL from {endpoint}"))?;
    if retry::graphql_value_is_transaction_conflict(&value) {
        return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
    }
    if graphql_response_has_errors(&value) {
        let errors = &value["errors"];
        anyhow::bail!("graphql returned errors: {errors}");
    }
    Ok(value)
}

pub(super) enum TxnCommitError {
    /// The request may not have reached DefraDB, so the transaction can still
    /// be open and needs best-effort cleanup.
    CleanupRequired(anyhow::Error),
    /// DefraDB returned a commit response. Its pinned transaction registry
    /// consumes the handle before attempting the storage commit.
    TransactionConsumed(anyhow::Error),
}

pub(super) async fn txn_commit(
    endpoint: &str,
    id: &str,
    client: &reqwest::Client,
) -> std::result::Result<(), TxnCommitError> {
    let api_base = graphql_api_base(endpoint).map_err(TxnCommitError::CleanupRequired)?;
    let response = client
        .post(format!("{api_base}/tx/{id}"))
        .send()
        .await
        .with_context(|| format!("posting tx commit to {endpoint}"))
        .map_err(TxnCommitError::CleanupRequired)?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .context("reading tx commit body")
        .map_err(TxnCommitError::TransactionConsumed)?;
    if status.is_success() {
        return Ok(());
    }
    let body = String::from_utf8_lossy(&bytes);
    if status == reqwest::StatusCode::CONFLICT && retry::is_transaction_conflict_text(&body) {
        return Err(TxnCommitError::TransactionConsumed(
            retry::transaction_conflict(ConflictSource::HttpStatusTextFallback),
        ));
    }
    Err(TxnCommitError::TransactionConsumed(anyhow::anyhow!(
        "tx commit returned HTTP {status} from {endpoint}: {body}"
    )))
}

pub(super) async fn txn_discard(endpoint: &str, id: &str, client: &reqwest::Client) -> Result<()> {
    let response = client
        .delete(format!("{}/tx/{id}", graphql_api_base(endpoint)?))
        .send()
        .await
        .with_context(|| format!("posting tx discard to {endpoint}"))?;
    let status = response.status();
    let bytes = response.bytes().await.context("reading tx discard body")?;
    if !status.is_success() {
        anyhow::bail!(
            "tx discard returned HTTP {status} from {endpoint}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}

pub(super) fn affected_documents(response: &Value) -> u64 {
    response
        .get("data")
        .and_then(Value::as_object)
        .map(|fields| {
            fields
                .values()
                .map(|value| match value {
                    Value::Array(rows) => rows.len() as u64,
                    Value::Object(_) => 1,
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or_default()
}

fn graphql_response_has_errors(response: &Value) -> bool {
    match response.get("errors") {
        None => false,
        Some(Value::Array(errors)) => !errors.is_empty(),
        Some(_) => true,
    }
}

pub(super) fn ensure_mutation_document(document: &str) -> Result<()> {
    if document.trim_start().starts_with("mutation") {
        Ok(())
    } else {
        anyhow::bail!("ConfigAccess::write requires a GraphQL mutation document")
    }
}

pub(crate) fn ensure_query_document(document: &str) -> Result<()> {
    let document = document.trim_start();
    if document.starts_with('{') || document.starts_with("query") {
        Ok(())
    } else {
        anyhow::bail!("ConfigAccess::execute requires a GraphQL query document")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transaction_http_preserves_variables_and_transaction_header() {
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        let expected =
            serde_json::json!({"input": {"a-b": [], "nested": [{"": {}, "雪": [null, []]}]}});
        let app =
            Router::new()
                .route(
                    "/graphql",
                    post(
                        |State(expected): State<Value>,
                         headers: HeaderMap,
                         Json(body): Json<Value>| async move {
                            assert_eq!(headers["x-defradb-tx"], "same-transaction");
                            assert_eq!(
                                body["query"],
                                "mutation($input: JSON) { test(input: $input) { _docID } }"
                            );
                            assert_eq!(body["variables"], expected);
                            Json(serde_json::json!({"data": {"test": [{"_docID": "written"}]}}))
                        },
                    ),
                )
                .with_state(expected.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/graphql", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let result = txn_execute(
            &endpoint,
            "same-transaction",
            &http_client().unwrap(),
            "mutation($input: JSON) { test(input: $input) { _docID } }",
            &expected,
        )
        .await;
        server.abort();
        assert_eq!(result.unwrap()["data"]["test"][0]["_docID"], "written");
    }

    #[test]
    fn counts_mutated_documents_in_graphql_envelope() {
        assert_eq!(
            affected_documents(&serde_json::json!({"data": {"add_X": [{}, {}]}})),
            2
        );
    }

    #[test]
    fn empty_graphql_error_array_is_success() {
        assert!(!graphql_response_has_errors(
            &serde_json::json!({"data": {"add_X": [{}]}, "errors": []})
        ));
        assert!(graphql_response_has_errors(
            &serde_json::json!({"errors": [{"message": "failed"}]})
        ));
        assert!(graphql_response_has_errors(
            &serde_json::json!({"errors": "malformed"})
        ));
    }

    #[test]
    fn rejects_queries_at_the_write_seam() {
        assert!(ensure_mutation_document("query { X { id } }").is_err());
    }

    #[test]
    fn rejects_mutations_at_the_query_seam() {
        assert!(ensure_query_document("mutation { update_X(input: {}) { id } }").is_err());
        assert!(ensure_query_document("query { X { id } }").is_ok());
        assert!(ensure_query_document("{ X { id } }").is_ok());
    }
}
