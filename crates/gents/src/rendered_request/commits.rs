//! DefraDB `_commits` constraints this helper encodes:
//! * `_commits` accepts exactly ONE `docID`; batched reads alias one field per
//!   document ([`field_commits_in_txn`]).
//! * Its `fieldName` filter is evaluated in memory and silently degrades to no
//!   filter on a malformed input, so no `fieldName` filter is sent; the field
//!   is selected in Rust instead.

use anyhow::{Context, Result};

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestJsonCommit {
    pub cid: String,
    pub height: i64,
}

pub async fn request_json_commit(
    access: &ConfigAccess,
    doc_id: &str,
) -> Result<Option<RequestJsonCommit>> {
    field_commit(access, doc_id, "request_json").await
}

pub async fn field_commit(
    access: &ConfigAccess,
    doc_id: &str,
    field_name: &str,
) -> Result<Option<RequestJsonCommit>> {
    let query = format!(
        r#"query {{ _commits(docID: "{doc_id}") {{ cid height fieldName }} }}"#,
        doc_id = escape_graphql_string(doc_id),
    );
    let response = access
        .execute(&query)
        .await
        .with_context(|| format!("reading _commits for rendered request {doc_id}"))?;
    select_field_commit(&response, field_name)
}

/// Maximum `_commits` aliases in one statement of [`field_commits_in_txn`],
/// under DefraDB's per-level field width guardrail; larger captures issue
/// further statements.
pub const FIELD_COMMIT_ALIAS_BATCH: usize = 32;

pub(crate) async fn field_commits<F, Fut>(
    doc_ids: &[String],
    field_name: &str,
    mut execute: F,
) -> Result<Vec<Option<RequestJsonCommit>>>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value>>,
{
    let mut commits = Vec::with_capacity(doc_ids.len());
    for batch in doc_ids.chunks(FIELD_COMMIT_ALIAS_BATCH) {
        let mut query = String::from("query {");
        for (index, doc_id) in batch.iter().enumerate() {
            query.push_str(&format!(
                r#" w{index}: _commits(docID: "{doc_id}") {{ cid height fieldName }}"#,
                doc_id = escape_graphql_string(doc_id),
            ));
        }
        query.push_str(" }");
        #[cfg(test)]
        let _ = FIELD_COMMIT_STATEMENTS
            .try_with(|statements| statements.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let response = execute(query).await?;
        commits.extend(select_batched_field_commits(&response, batch, field_name)?);
    }
    Ok(commits)
}

#[cfg(test)]
tokio::task_local! {
    static FIELD_COMMIT_STATEMENTS: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(test)]
pub(crate) async fn count_field_commit_statements<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let statements = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = FIELD_COMMIT_STATEMENTS
        .scope(statements.clone(), future)
        .await;
    (
        output,
        statements.load(std::sync::atomic::Ordering::Relaxed),
    )
}

pub(crate) async fn field_commits_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    doc_ids: &[String],
    field_name: &str,
) -> Result<Vec<Option<RequestJsonCommit>>> {
    field_commits(doc_ids, field_name, |query| async move {
        txn.execute(&query)
            .await
            .context("reading batched _commits in transaction")
    })
    .await
}

fn select_batched_field_commits(
    response: &serde_json::Value,
    batch: &[String],
    field_name: &str,
) -> Result<Vec<Option<RequestJsonCommit>>> {
    let data = response
        .get("data")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("batched _commits response carried no data object"))?;
    batch
        .iter()
        .enumerate()
        .map(|(index, doc_id)| {
            let commits = data
                .get(&format!("w{index}"))
                .and_then(serde_json::Value::as_array)
                .with_context(|| {
                    format!("batched _commits response carried no array for {doc_id}")
                })?;
            Ok(highest_field_commit(commits, field_name))
        })
        .collect()
}

#[cfg(test)]
fn select_request_json_commit(response: &serde_json::Value) -> Result<Option<RequestJsonCommit>> {
    select_field_commit(response, "request_json")
}

pub(super) fn select_field_commit(
    response: &serde_json::Value,
    field_name: &str,
) -> Result<Option<RequestJsonCommit>> {
    let commits = response
        .get("data")
        .and_then(|data| data.get("_commits"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("_commits response carried no data._commits array"))?;
    Ok(highest_field_commit(commits, field_name))
}

fn highest_field_commit(
    commits: &[serde_json::Value],
    field_name: &str,
) -> Option<RequestJsonCommit> {
    commits
        .iter()
        .filter(|commit| {
            commit.get("fieldName").and_then(serde_json::Value::as_str) == Some(field_name)
        })
        .filter_map(|commit| {
            Some(RequestJsonCommit {
                cid: commit.get("cid")?.as_str()?.to_string(),
                height: commit.get("height").and_then(serde_json::Value::as_i64)?,
            })
        })
        .max_by(|left, right| (left.height, &left.cid).cmp(&(right.height, &right.cid)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn selects_the_highest_request_json_commit() {
        let response = json!({
            "data": {
                "_commits": [
                    { "cid": "bafy-composite", "height": 3, "fieldName": "_C" },
                    { "cid": "bafy-old", "height": 1, "fieldName": "request_json" },
                    { "cid": "bafy-current", "height": 2, "fieldName": "request_json" },
                    { "cid": "bafy-prov", "height": 2, "fieldName": "provenance_json" },
                ]
            }
        });
        assert_eq!(
            select_request_json_commit(&response).unwrap(),
            Some(RequestJsonCommit {
                cid: "bafy-current".to_string(),
                height: 2,
            })
        );
    }

    #[test]
    fn commits_without_a_request_json_field_are_unavailable() {
        let response = json!({
            "data": {
                "_commits": [
                    { "cid": "bafy-composite", "height": 1, "fieldName": "_C" },
                ]
            }
        });
        assert_eq!(select_request_json_commit(&response).unwrap(), None);
    }

    #[test]
    fn an_empty_commit_list_is_unavailable_never_unchanged() {
        let response = json!({ "data": { "_commits": [] } });
        assert_eq!(select_request_json_commit(&response).unwrap(), None);
    }

    #[test]
    fn a_shapeless_response_is_an_error() {
        assert!(select_request_json_commit(&json!({})).is_err());
        assert!(select_request_json_commit(&json!({ "data": {} })).is_err());
    }

    fn batch(doc_ids: &[&str]) -> Vec<String> {
        doc_ids.iter().map(|doc_id| doc_id.to_string()).collect()
    }

    #[test]
    fn batched_selection_answers_per_document_in_order() {
        let response = json!({
            "data": {
                "w0": [
                    { "cid": "bafy-composite", "height": 3, "fieldName": "_C" },
                    { "cid": "bafy-old", "height": 1, "fieldName": "payload" },
                    { "cid": "bafy-current", "height": 2, "fieldName": "payload" },
                ],
                "w1": [],
                "w2": [
                    { "cid": "bafy-other-field", "height": 5, "fieldName": "content_key" },
                ],
            }
        });
        let selected = select_batched_field_commits(
            &response,
            &batch(&["doc-a", "doc-b", "doc-c"]),
            "payload",
        )
        .unwrap();
        assert_eq!(
            selected,
            vec![
                Some(RequestJsonCommit {
                    cid: "bafy-current".into(),
                    height: 2
                }),
                None,
                None,
            ]
        );
    }

    #[test]
    fn a_dropped_alias_is_an_error_for_its_document() {
        let response = json!({ "data": { "w0": [], "w2": [] } });
        let error = select_batched_field_commits(
            &response,
            &batch(&["doc-a", "doc-b", "doc-c"]),
            "payload",
        )
        .unwrap_err();
        assert!(error.to_string().contains("doc-b"), "{error:#}");

        let null_alias = json!({ "data": { "w0": null } });
        assert!(select_batched_field_commits(&null_alias, &batch(&["doc-a"]), "payload").is_err());
        assert!(select_batched_field_commits(&json!({}), &batch(&["doc-a"]), "payload").is_err());
    }
}
