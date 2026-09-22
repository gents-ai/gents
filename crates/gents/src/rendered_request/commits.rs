//! The one implementation of rendered-request field-commit reads.
//!
//! For capture v2 the CID witnesses the stored capture container, while a
//! logical delta recursively pins the CID of every base container needed to recover the
//! canonical value. Integrity comes from that witnessed chain, not from a
//! writer-supplied digest. Reading a field commit has two traps every consumer
//! would otherwise rediscover:
//!
//! * `_commits` accepts exactly ONE `docID`; two or more is a parse error.
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
use defra_node::EmbeddedNode;

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

pub(crate) async fn field_commit_embedded(
    node: &EmbeddedNode,
    doc_id: &str,
    field_name: &str,
) -> Result<Option<RequestJsonCommit>> {
    let query = format!(
        r#"query {{ _commits(docID: "{doc_id}") {{ cid height fieldName }} }}"#,
        doc_id = escape_graphql_string(doc_id),
    );
    let response = node.execute(&query).await;
    crate::graphql::ensure_no_errors(&response, "reading rendered-request field commit")?;
    select_field_commit(&serde_json::json!({"data": response.data}), field_name)
}

/// Pure selection over a `_commits` response: pick the highest
/// `request_json` field commit, in Rust rather than in a query filter.
#[cfg(test)]
fn select_request_json_commit(response: &serde_json::Value) -> Result<Option<RequestJsonCommit>> {
    select_field_commit(response, "request_json")
}

fn select_field_commit(
    response: &serde_json::Value,
    field_name: &str,
) -> Result<Option<RequestJsonCommit>> {
    let commits = response
        .get("data")
        .and_then(|data| data.get("_commits"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("_commits response carried no data._commits array"))?;

    Ok(commits
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
        .max_by(|left, right| (left.height, &left.cid).cmp(&(right.height, &right.cid))))
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
}
