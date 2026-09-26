//! Session origin: which request began a session, and what caused it.
//!
//! A session is caused by another request only when its origin (its first
//! public request under the canonical request order, `created_at` then
//! `request_id`) carries that request in `caused_by_parent_request_doc_id`.
//! A message appended to an existing session carries the same edge but never
//! makes that session a caused session. Every lineage reader applies this
//! rule through this owner.
//!
//! Reads are scoped to public requests and exact session scopes. Listing
//! reads carry no `limit`, so they are never truncated; the only limited
//! reads are unordered existence probes and exact `_docID` reads.

use std::collections::HashSet;

use anyhow::Result;
use serde_json::Value;

use crate::config_client::ConfigAccess;
use crate::defra_node::EmbeddedNode;
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::session::{public_request_filter, session_scope_filter};

/// Where origin reads run: the embedded node, or any configuration access
/// (local or HTTP GraphQL).
#[derive(Clone, Copy)]
pub enum OriginReader<'a> {
    Node(&'a EmbeddedNode),
    Access(&'a ConfigAccess),
}

impl OriginReader<'_> {
    /// Run one read and return its `data` object.
    pub async fn data(&self, query: &str, operation: &str) -> Result<Value> {
        match self {
            Self::Node(node) => Ok(graphql_with_transaction_retry(node, query, operation)
                .await?
                .data
                .unwrap_or(Value::Null)),
            Self::Access(access) => Ok(access
                .execute(query)
                .await?
                .get("data")
                .cloned()
                .unwrap_or(Value::Null)),
        }
    }
}

/// Fields every origin read selects: identity, scope and the causing edge.
pub const ORIGIN_FIELDS: &str = "_docID request_id agent_did session_id requester_did \
     behavior_id created_at lifecycle_state caused_by_parent_request_id \
     caused_by_parent_request_doc_id caused_by_parent_tool_call_id \
     caused_by_parent_tool_call_doc_id";

/// The exact scope of one session label.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionScope {
    pub agent_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
}

impl SessionScope {
    /// The scope of an `AgentRequest` row read with [`ORIGIN_FIELDS`].
    pub fn of_row(row: &Value) -> Option<Self> {
        Some(Self {
            agent_did: row.get("agent_did")?.as_str()?.to_string(),
            session_id: row.get("session_id")?.as_str()?.to_string(),
            requester_did: row
                .get("requester_did")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        })
    }
}

/// The origins of the sessions caused by any of `cause_doc_ids`, as raw
/// `AgentRequest` rows with [`ORIGIN_FIELDS`] plus `extra_fields`.
pub async fn load_session_origins(
    reader: OriginReader<'_>,
    cause_doc_ids: &[String],
    extra_fields: &str,
) -> Result<Vec<Value>> {
    if cause_doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let filter = public_request_filter(&format!(
        "caused_by_parent_request_doc_id: {{ _in: [{}] }}",
        graphql_string_list(cause_doc_ids)
    ));
    let query = format!("{{AgentRequest(filter:{{{filter}}}){{{ORIGIN_FIELDS} {extra_fields}}}}}");
    let rows = request_rows(&reader.data(&query, "session origin candidates").await?);
    retain_session_origins(reader, rows).await
}

/// The caused origin of `session_id` in each scope that has one. More than
/// one row means the label is ambiguous across scopes.
pub async fn load_caused_session_origins(
    reader: OriginReader<'_>,
    session_id: &str,
    extra_fields: &str,
) -> Result<Vec<Value>> {
    let filter = public_request_filter(&format!(
        r#"session_id: {{ _eq: "{}" }}, caused_by_parent_request_doc_id: {{ _ne: null }}"#,
        escape_graphql_string(session_id)
    ));
    let query = format!("{{AgentRequest(filter:{{{filter}}}){{{ORIGIN_FIELDS} {extra_fields}}}}}");
    let rows = request_rows(&reader.data(&query, "caused session origin").await?);
    retain_session_origins(reader, rows).await
}

/// Keep only the rows that are the first public request of their session.
/// Rows must carry `agent_did`, `session_id`, `requester_did`, `created_at`
/// and `request_id`; a row missing any of them is not an origin.
pub async fn retain_session_origins(
    reader: OriginReader<'_>,
    rows: Vec<Value>,
) -> Result<Vec<Value>> {
    let mut probes = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (Some(scope), Some(created_at), Some(request_id)) = (
            SessionScope::of_row(row),
            row.get("created_at").and_then(Value::as_str),
            row.get("request_id").and_then(Value::as_str),
        ) else {
            continue;
        };
        let scope_filter = public_request_filter(&session_scope_filter(
            &scope.agent_did,
            &scope.session_id,
            scope.requester_did.as_deref(),
        ));
        let created_at = escape_graphql_string(created_at);
        let request_id = escape_graphql_string(request_id);
        // No `order`: an ordered read may apply `limit` before the filter.
        probes.push((
            index,
            format!(
                r#"earlier{index}: AgentRequest(filter: {{ {scope_filter}, _or: [{{ created_at: {{ _lt: "{created_at}" }} }}, {{ created_at: {{ _eq: "{created_at}" }}, request_id: {{ _lt: "{request_id}" }} }}] }}, limit: 1) {{ _docID }}"#
            ),
        ));
    }
    if probes.is_empty() {
        return Ok(Vec::new());
    }
    let query = format!(
        "{{{}}}",
        probes
            .iter()
            .map(|(_, probe)| probe.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let data = reader.data(&query, "session origin check").await?;
    let origins = probes
        .iter()
        .filter(|(index, _)| {
            data.get(format!("earlier{index}"))
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        })
        .map(|(index, _)| *index)
        .collect::<HashSet<_>>();
    Ok(rows
        .into_iter()
        .enumerate()
        .filter(|(index, _)| origins.contains(index))
        .map(|(_, row)| row)
        .collect())
}

/// Physical identities of every public request in the given sessions, with
/// the scope each belongs to.
pub async fn load_session_request_doc_ids(
    reader: OriginReader<'_>,
    scopes: &[SessionScope],
) -> Result<Vec<(String, SessionScope)>> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let filters = scopes
        .iter()
        .map(|scope| {
            format!(
                "{{{}}}",
                public_request_filter(&session_scope_filter(
                    &scope.agent_did,
                    &scope.session_id,
                    scope.requester_did.as_deref(),
                ))
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "{{AgentRequest(filter:{{_or:[{filters}]}}){{_docID agent_did session_id requester_did}}}}"
    );
    Ok(
        request_rows(&reader.data(&query, "session request identities").await?)
            .iter()
            .filter_map(|row| {
                let doc_id = row.get("_docID")?.as_str()?.to_string();
                let scope = SessionScope::of_row(row)?;
                scopes.contains(&scope).then_some((doc_id, scope))
            })
            .collect(),
    )
}

/// The scope of one physical request, when it is visible.
pub async fn load_request_scope(
    reader: OriginReader<'_>,
    request_doc_id: &str,
) -> Result<Option<SessionScope>> {
    // The exact `_docID` read carries no `order`, so `limit` cannot hide it.
    let query = format!(
        r#"{{AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID agent_did session_id requester_did }}}}"#,
        escape_graphql_string(request_doc_id)
    );
    let mut rows = request_rows(&reader.data(&query, "request scope").await?);
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous physical request {request_doc_id}"
    );
    Ok(rows.pop().as_ref().and_then(SessionScope::of_row))
}

fn request_rows(data: &Value) -> Vec<Value> {
    data.get("AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn graphql_string_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        created_at: &str,
        cause_doc_id: Option<&str>,
    ) -> String {
        let cause = cause_doc_id
            .map(|doc_id| {
                format!(
                    r#"caused_by_parent_request_id: "p1", caused_by_parent_request_doc_id: "{}", caused_by_parent_tool_call_id: "call-1","#,
                    escape_graphql_string(doc_id)
                )
            })
            .unwrap_or_default();
        let response = node
            .execute(&format!(
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", agent_did: "did:test:agent", requester_did: "did:test:agent", behavior_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{created_at}", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = OriginReader::Node(node)
            .data(
                &format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{request_id}"}}}}) {{_docID}}}}"#),
                "seed readback",
            )
            .await
            .unwrap();
        request_rows(&data)[0]["_docID"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn request_ids(rows: &[Value]) -> Vec<&str> {
        rows.iter()
            .map(|row| row["request_id"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_message_into_an_existing_session_does_not_make_it_caused() {
        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let parent = seed(&node, "p1", "parent", "2026-09-26T00:00:01Z", None).await;
        seed(&node, "s1", "existing", "2026-09-26T00:00:00Z", None).await;
        seed(
            &node,
            "s2",
            "existing",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;
        seed(
            &node,
            "c1",
            "started",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;
        seed(
            &node,
            "c2",
            "started",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;

        let reader = OriginReader::Node(&node);
        let origins = load_session_origins(reader, &[parent.clone()], "")
            .await
            .unwrap();
        assert_eq!(request_ids(&origins), ["c1"]);
        assert!(load_caused_session_origins(reader, "existing", "")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            request_ids(
                &load_caused_session_origins(reader, "started", "")
                    .await
                    .unwrap()
            ),
            ["c1"]
        );
        let access = ConfigAccess::Local(node.clone());
        assert_eq!(
            request_ids(
                &load_session_origins(OriginReader::Access(&access), &[parent], "")
                    .await
                    .unwrap()
            ),
            ["c1"]
        );
        node.shutdown().await;
    }
}
