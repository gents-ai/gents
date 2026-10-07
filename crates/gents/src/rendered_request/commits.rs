//! The one implementation of rendered-request field-commit reads.
//!
//! For capture v2 the CID witnesses the stored capture container, while a
//! logical delta recursively pins the CID of every base container needed to recover the
//! canonical value. Integrity comes from that witnessed chain, not from a
//! writer-supplied digest. Reading a field commit has two traps every consumer
//! would otherwise rediscover:
//!
//! * `_commits` accepts exactly ONE `docID`; two or more is a parse error.
//!   Batched reads therefore alias one `_commits` field per document
//!   ([`field_commits_in_txn`]) instead of handing several docIDs to one
//!   field.
//! * Its `fieldName` filter is evaluated **in memory**, with
//!   `filter.matches(..).unwrap_or(true)` — a malformed filter silently
//!   degrades to *no* filter, which combined with `limit: 1` returns an
//!   arbitrary commit. So this helper sends no `fieldName` filter at all and
//!   selects the `request_json` commit in Rust, where a mistake is a type
//!   error instead of a wrong answer.
//!
//! `[]` from `_commits` is reported as explicit *Unavailable* (`Ok(None)`),
//! never "unchanged"; a GraphQL error is an error.

use anyhow::{Context, Result};

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;

/// The field-commit witness for a stored rendered-request field value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestJsonCommit {
    pub cid: String,
    pub height: i64,
}

/// Read the current `request_json` field-commit CID for one `RenderedRequest`
/// document. `Ok(None)` is explicit *Unavailable*: the document has no commits
/// visible on this node, or none for `request_json`.
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

/// Maximum `_commits` aliases in one statement of
/// [`field_commits_in_txn`]. 32 matches the repo's other aliased batch
/// (`persist_prepared_documents`), which sits under DefraDB's per-level field
/// width guardrail (100 fields at the pinned rev); a capture with more block
/// documents issues further statements rather than one unbounded query.
pub const FIELD_COMMIT_ALIAS_BATCH: usize = 32;

/// Read the field commits of many documents inside the caller's transaction,
/// one aliased `_commits` field per document and at most
/// [`FIELD_COMMIT_ALIAS_BATCH`] aliases per statement.
///
/// The capture sink uses this to pin the `payload` witness of every block
/// document in the same transaction that creates the blocks and the manifest
/// referencing them, so the pinned CIDs and the block writes commit as one
/// unit; the CID a transaction observes for its own create is the CID every
/// later reader observes, which is what makes the pin meaningful. Reading
/// them batched changes only the round-trip count: same snapshot, same
/// per-document selection discipline (the `fieldName` match happens in Rust
/// over the returned commits), and results answering one-for-one in `doc_ids`
/// order, so a caller that errors on `None` per entry keeps exactly the
/// guarantees one-read-per-document gave. A response that omits an alias, or
/// answers one with something other than a commits array, is an error — never
/// a silently skipped document.
pub(crate) async fn field_commits_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    doc_ids: &[String],
    field_name: &str,
) -> Result<Vec<Option<RequestJsonCommit>>> {
    let mut commits = Vec::with_capacity(doc_ids.len());
    for batch in doc_ids.chunks(FIELD_COMMIT_ALIAS_BATCH) {
        if batch.is_empty() {
            continue;
        }
        let mut query = String::from("query {");
        for (index, doc_id) in batch.iter().enumerate() {
            query.push_str(&format!(
                r#" w{index}: _commits(docID: "{doc_id}") {{ cid height fieldName }}"#,
                doc_id = escape_graphql_string(doc_id),
            ));
        }
        query.push_str(" }");
        let response = txn
            .execute(&query)
            .await
            .with_context(|| "reading batched _commits in transaction")?;
        commits.extend(select_batched_field_commits(&response, batch, field_name)?);
    }
    Ok(commits)
}

/// Pure selection over one batched `_commits` response: for every document of
/// the batch, in order, the highest commit for `field_name` under its alias.
/// An alias the response omits, or answers with anything but a commits array,
/// is an error — the batched read may not silently drop a document.
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

/// Pure selection over a `_commits` response: pick the highest
/// `request_json` field commit, in Rust rather than in a query filter.
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

/// The highest commit for `field_name` within one `_commits` result array, in
/// Rust rather than in a query filter. A commit without the named field, a
/// `cid` or a `height` is not a candidate; no candidates is `None`.
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

    /// Commits exist, but none for `request_json` — that is Unavailable, not
    /// "take whichever commit came back", which is exactly the in-memory
    /// filter failure mode this helper exists to rule out.
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

    /// A response with no `data._commits` array at all is an error — a missing
    /// `data` field is not the same statement as "no rows".
    #[test]
    fn a_shapeless_response_is_an_error() {
        assert!(select_request_json_commit(&json!({})).is_err());
        assert!(select_request_json_commit(&json!({ "data": {} })).is_err());
    }

    fn batch(doc_ids: &[&str]) -> Vec<String> {
        doc_ids.iter().map(|doc_id| doc_id.to_string()).collect()
    }

    /// The batched selection answers one-for-one in batch order, applying the
    /// same per-document discipline as the single read: highest commit for the
    /// field, `None` for a document with none.
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

    /// An alias the response dropped is an error naming the document, never a
    /// silently skipped entry — the batched read must not weaken the per-entry
    /// verification the single reads gave.
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
