use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;

use crate::config_client::{ConfigAccess, ConfigRead};
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::session::{
    decode_session_row, public_request_filter, session_scope_filter, AGENT_SESSION_FIELDS,
};
use gents_protocol::session::AgentSession;

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionScope {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
}

impl SessionScope {
    pub fn of_request(request: &crate::AgentRequest) -> Self {
        Self {
            node_did: request.node_did.clone(),
            session_id: request.session_id.clone(),
            requester_did: request.requester_did.clone(),
        }
    }

    fn of_row(row: &Value) -> Option<Self> {
        Some(Self {
            node_did: row.get("node_did")?.as_str()?.to_string(),
            session_id: row.get("session_id")?.as_str()?.to_string(),
            requester_did: row
                .get("requester_did")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLink {
    pub scope: SessionScope,
    pub cause_request_doc_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRequestCause {
    pub request_id: String,
    pub cause_request_doc_id: String,
    pub cause_scope: Option<SessionScope>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionLineage {
    pub started_by: Option<SessionLink>,
    pub started: Vec<SessionLink>,
    pub sent: Vec<SessionLink>,
    pub received: Vec<SessionLink>,
    pub request_causes: Vec<SessionRequestCause>,
}

const SCOPE_FIELDS: &str = "_docID node_did session_id requester_did";

async fn collection<R: ConfigRead + ?Sized>(
    access: &R,
    query: &str,
    name: &str,
) -> Result<Vec<Value>> {
    Ok(access
        .execute_read(query)
        .await?
        .pointer(&format!("/data/{name}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

async fn sessions(access: &ConfigAccess, filter: &str) -> Result<Vec<AgentSession>> {
    collection(
        access,
        &format!("{{AgentSession(filter: {{{filter}}}) {{{AGENT_SESSION_FIELDS}}}}}"),
        "AgentSession",
    )
    .await?
    .iter()
    .map(|row| decode_session_row(row).map(|row| row.session))
    .collect()
}

fn provenance_parent(session: &AgentSession) -> Option<&str> {
    session
        .provenance
        .as_ref()?
        .parent_request_doc_id
        .as_deref()
}

impl From<&AgentSession> for SessionScope {
    fn from(session: &AgentSession) -> Self {
        Self {
            node_did: session.node_did.clone(),
            session_id: session.session_id.clone(),
            requester_did: session.requester_did.clone(),
        }
    }
}

pub async fn request_scopes<'a, R: ConfigRead + ?Sized>(
    access: &R,
    doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<(String, SessionScope)>> {
    let doc_ids = doc_ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut scopes = Vec::new();
    for batch in doc_ids.chunks(128) {
        let query = format!(
            "{{AgentRequest(docIDs: {}) {{{SCOPE_FIELDS}}}}}",
            graphql_string_list_literal(batch.iter().copied())
        );
        scopes.extend(
            collection(access, &query, "AgentRequest")
                .await?
                .iter()
                .filter_map(|row| {
                    Some((
                        row.get("_docID")?.as_str()?.to_owned(),
                        SessionScope::of_row(row)?,
                    ))
                }),
        );
    }
    Ok(scopes)
}

pub async fn load_session(
    access: &ConfigAccess,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<AgentSession>> {
    let mut filter = format!(
        r#"session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(session_id)
    );
    if let Some(requester) = requester_did {
        filter.push_str(&format!(
            r#", requester_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(requester)
        ));
    }
    let mut rows = sessions(access, &filter).await?;
    anyhow::ensure!(rows.len() <= 1, "session {session_id} is ambiguous");
    Ok(rows.pop())
}

pub async fn started_by(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> Result<Option<SessionLink>> {
    let own = sessions(
        access,
        &session_scope_filter(
            &scope.node_did,
            &scope.session_id,
            scope.requester_did.as_deref(),
        ),
    )
    .await?;
    let Some(doc_id) = own.first().and_then(provenance_parent) else {
        return Ok(None);
    };
    Ok(request_scopes(access, [doc_id])
        .await?
        .into_iter()
        .next()
        .map(|(cause, scope)| SessionLink {
            scope,
            cause_request_doc_id: cause,
        }))
}

/// One public request a physical tool call caused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CausedRequest {
    pub request_id: String,
    pub request_doc_id: String,
    pub scope: SessionScope,
    pub created_at: String,
}

/// Group request rows by `caused_by_parent_tool_call_doc_id`, each list
/// ordered by `(created_at, request_doc_id)`.
pub fn group_caused(rows: &[Value]) -> BTreeMap<String, Vec<CausedRequest>> {
    let mut grouped = BTreeMap::<String, Vec<CausedRequest>>::new();
    for row in rows {
        let text = |field: &str| row.get(field).and_then(Value::as_str).map(str::to_owned);
        let (Some(call), Some(request_id), Some(request_doc_id), Some(created_at), Some(scope)) = (
            text("caused_by_parent_tool_call_doc_id"),
            text("request_id"),
            text("_docID"),
            text("created_at"),
            SessionScope::of_row(row),
        ) else {
            continue;
        };
        grouped.entry(call).or_default().push(CausedRequest {
            request_id,
            request_doc_id,
            scope,
            created_at,
        });
    }
    for requests in grouped.values_mut() {
        requests.sort_by(|a, b| {
            (&a.created_at, &a.request_doc_id).cmp(&(&b.created_at, &b.request_doc_id))
        });
    }
    grouped
}

/// Every public request each physical tool call caused: the requests whose
/// `caused_by_parent_tool_call_doc_id` names that call.
pub async fn requests_caused_by_calls<'a>(
    access: &ConfigAccess,
    call_doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, Vec<CausedRequest>>> {
    let call_doc_ids = call_doc_ids.into_iter().collect::<BTreeSet<_>>();
    if call_doc_ids.is_empty() {
        return Ok(Default::default());
    }
    let query = format!(
        "{{AgentRequest(filter: {{{}}}) {{{SCOPE_FIELDS} request_id created_at caused_by_parent_tool_call_doc_id}}}}",
        public_request_filter(&format!(
            "caused_by_parent_tool_call_doc_id: {{ _in: {} }}",
            graphql_string_list_literal(call_doc_ids)
        ))
    );
    Ok(group_caused(
        &collection(access, &query, "AgentRequest").await?,
    ))
}

/// The single-request projection, for session-message calls (`agent_new`,
/// `agent_message`); one call may cause many requests
/// ([`requests_caused_by_calls`]). A call named by more than one request has
/// no single caused request and is omitted.
pub async fn caused_requests<'a>(
    access: &ConfigAccess,
    tool_call_doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>> {
    Ok(requests_caused_by_calls(access, tool_call_doc_ids)
        .await?
        .into_iter()
        .filter_map(
            |(call, requests)| match <[CausedRequest; 1]>::try_from(requests) {
                Ok([only]) => Some((call, only.request_id)),
                Err(_) => None,
            },
        )
        .collect())
}

pub async fn lineage(access: &ConfigAccess, scope: &SessionScope) -> Result<SessionLineage> {
    let own_filter = session_scope_filter(
        &scope.node_did,
        &scope.session_id,
        scope.requester_did.as_deref(),
    );
    let started_by = started_by(access, scope).await?;

    let own_requests = collection(
        access,
        &format!(
            "{{AgentRequest(filter: {{{}}}) {{{SCOPE_FIELDS} request_id caused_by_parent_request_doc_id}}}}",
            public_request_filter(&own_filter)
        ),
        "AgentRequest",
    )
    .await?;
    let own_doc_ids = own_requests
        .iter()
        .filter_map(|row| row.get("_docID")?.as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let causes = own_requests
        .iter()
        .filter_map(|row| row.get("caused_by_parent_request_doc_id")?.as_str())
        .filter(|doc_id| !own_doc_ids.contains(*doc_id))
        .collect::<BTreeSet<_>>();
    let cause_scopes = request_scopes(access, causes).await?;
    let cause_scopes_by_doc = cause_scopes
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    let request_causes = own_requests
        .iter()
        .filter_map(|row| {
            Some(SessionRequestCause {
                request_id: row.get("request_id")?.as_str()?.to_owned(),
                cause_request_doc_id: row
                    .get("caused_by_parent_request_doc_id")?
                    .as_str()?
                    .to_owned(),
                cause_scope: cause_scopes_by_doc
                    .get(row.get("caused_by_parent_request_doc_id")?.as_str()?)
                    .cloned(),
            })
        })
        .collect::<Vec<_>>();
    let mut received = Vec::new();
    for (cause, cause_scope) in cause_scopes {
        if cause_scope == *scope
            || started_by
                .as_ref()
                .is_some_and(|link| link.scope == cause_scope)
            || received
                .iter()
                .any(|link: &SessionLink| link.scope == cause_scope)
        {
            continue;
        }
        received.push(SessionLink {
            scope: cause_scope,
            cause_request_doc_id: cause,
        });
    }
    if own_doc_ids.is_empty() {
        return Ok(SessionLineage {
            started_by,
            received,
            request_causes,
            ..Default::default()
        });
    }

    let mut started = Vec::new();
    for session in sessions(
        access,
        &format!(
            r#"requester_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(&scope.node_did)
        ),
    )
    .await?
    {
        let Some(cause) =
            provenance_parent(&session).filter(|doc_id| own_doc_ids.contains(*doc_id))
        else {
            continue;
        };
        started.push(SessionLink {
            scope: SessionScope::from(&session),
            cause_request_doc_id: cause.to_owned(),
        });
    }

    let mut sent = Vec::new();
    let query = format!(
        "{{AgentRequest(filter: {{{}}}) {{{SCOPE_FIELDS} caused_by_parent_request_doc_id}}}}",
        public_request_filter(&format!(
            "caused_by_parent_request_doc_id: {{ _in: {} }}",
            graphql_string_list_literal(own_doc_ids.iter().map(String::as_str))
        ))
    );
    for row in collection(access, &query, "AgentRequest").await? {
        let (Some(target), Some(cause)) = (
            SessionScope::of_row(&row),
            row.get("caused_by_parent_request_doc_id")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        if target == *scope
            || started.iter().any(|link| link.scope == target)
            || sent.iter().any(|link: &SessionLink| link.scope == target)
        {
            continue;
        }
        sent.push(SessionLink {
            scope: target,
            cause_request_doc_id: cause.to_owned(),
        });
    }
    Ok(SessionLineage {
        started_by,
        started,
        sent,
        received,
        request_causes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defra_node::EmbeddedNode;
    use gents_protocol::session::SessionProvenance;

    async fn create(node: &EmbeddedNode, mutation: String, collection: &str) -> String {
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        crate::graphql::single_mutation_document(&response, &format!("create_{collection}"))
            .unwrap()
            .and_then(|document| document["_docID"].as_str())
            .unwrap()
            .to_owned()
    }

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
        create(
            node,
            format!(
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "did:test:node", requester_did: "did:test:node", agent_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{created_at}", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            ),
            "AgentRequest",
        )
        .await
    }

    async fn seed_session(node: &EmbeddedNode, session_id: &str, parent: Option<&str>) {
        let session = AgentSession {
            session_id: session_id.into(),
            node_did: "did:test:node".into(),
            requester_did: Some("did:test:node".into()),
            agent_id: "worker".into(),
            created_at: "2026-09-26T00:00:00Z".into(),
            closed_at: None,
            title: None,
            tags: Vec::new(),
            provenance: parent.map(|doc_id| SessionProvenance {
                parent_request_doc_id: Some(doc_id.into()),
                ..Default::default()
            }),
            observation: None,
        };
        let input = gents_protocol::graphql::graphql_input_literal(
            &serde_json::to_value(&session).unwrap(),
        )
        .unwrap();
        create(
            node,
            format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
            "AgentSession",
        )
        .await;
    }

    fn caused_row(doc_id: &str, request_id: &str, created_at: &str, call: &str) -> Value {
        serde_json::json!({
            "_docID": doc_id,
            "request_id": request_id,
            "node_did": "did:test:node",
            "session_id": "child",
            "requester_did": "did:test:node",
            "created_at": created_at,
            "caused_by_parent_request_doc_id": "bae-parent",
            "caused_by_parent_tool_call_doc_id": call,
        })
    }

    #[test]
    fn group_caused_orders_by_created_at_then_doc_id() {
        let grouped = group_caused(&[
            caused_row("bae-c", "r3", "2026-09-26T00:00:02Z", "call-1"),
            caused_row("bae-b", "r2", "2026-09-26T00:00:01Z", "call-1"),
            caused_row("bae-a", "r1", "2026-09-26T00:00:01Z", "call-1"),
        ]);
        let doc_ids = grouped["call-1"]
            .iter()
            .map(|request| request.request_doc_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(doc_ids, ["bae-a", "bae-b", "bae-c"]);
    }

    #[test]
    fn group_caused_keeps_every_request_of_one_call() {
        let grouped = group_caused(&[
            caused_row("bae-a", "r1", "2026-09-26T00:00:01Z", "call-1"),
            caused_row("bae-b", "r2", "2026-09-26T00:00:02Z", "call-1"),
            caused_row("bae-c", "r3", "2026-09-26T00:00:03Z", "call-2"),
        ]);
        let ids = |call: &str| {
            grouped[call]
                .iter()
                .map(|request| request.request_id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("call-1"), ["r1", "r2"]);
        assert_eq!(ids("call-2"), ["r3"]);
        assert_eq!(grouped["call-1"][0].scope, scope("child"));
    }

    #[tokio::test]
    async fn caused_requests_projection_omits_a_call_with_two_requests() {
        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        for (request_id, call) in [("a1", "call-one"), ("b1", "call-two"), ("b2", "call-two")] {
            create(
                &node,
                format!(
                    r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "did:test:node", requester_did: "did:test:node", agent_id: "worker", session_id: "child", caused_by_parent_request_id: "p1", caused_by_parent_request_doc_id: "bae-parent", caused_by_parent_tool_call_id: "{call}", caused_by_parent_tool_call_doc_id: "{call}", content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "2026-09-26T00:00:01Z", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
                ),
                "AgentRequest",
            )
            .await;
        }
        let access = ConfigAccess::local(node.clone());
        let projection = caused_requests(&access, ["call-one", "call-two"])
            .await
            .unwrap();
        assert_eq!(
            projection,
            std::collections::BTreeMap::from([("call-one".to_owned(), "a1".to_owned())])
        );
        node.shutdown().await;
    }

    fn scope(session_id: &str) -> SessionScope {
        SessionScope {
            node_did: "did:test:node".into(),
            session_id: session_id.into(),
            requester_did: Some("did:test:node".into()),
        }
    }

    fn ids(links: &[SessionLink]) -> Vec<&str> {
        links
            .iter()
            .map(|link| link.scope.session_id.as_str())
            .collect()
    }

    #[tokio::test]
    async fn physical_request_scope_batch_keeps_requested_missing_and_foreign_rows_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let requested = seed(
            &node,
            "requested",
            "requested-session",
            "2026-09-26T00:00:00Z",
            None,
        )
        .await;
        let _foreign = seed(
            &node,
            "foreign",
            "foreign-session",
            "2026-09-26T00:00:01Z",
            None,
        )
        .await;
        let access = ConfigAccess::local(node.clone());

        let missing =
            document::DocID::new_v0_from_seed("missing-session-origin-request").to_string();
        let scopes = request_scopes(&access, [requested.as_str(), missing.as_str()])
            .await
            .unwrap();
        assert_eq!(
            scopes,
            vec![(requested, scope("requested-session"))],
            "the direct-ID read must return only found requested documents with their stored scope"
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn a_message_into_an_existing_session_does_not_make_it_started() {
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
        seed_session(&node, "parent", None).await;
        seed(&node, "s1", "existing", "2026-09-26T00:00:00Z", None).await;
        seed(
            &node,
            "s2",
            "existing",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;
        seed_session(&node, "existing", None).await;
        seed(
            &node,
            "c1",
            "started",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;
        seed_session(&node, "started", Some(&parent)).await;
        let other = seed(&node, "o1", "other", "2026-09-26T00:00:01Z", None).await;
        seed_session(&node, "other", None).await;
        seed(&node, "a0", "started", "2026-09-26T00:00:02Z", Some(&other)).await;
        let access = ConfigAccess::local(node.clone());

        let from_parent = lineage(&access, &scope("parent")).await.unwrap();
        assert_eq!(from_parent.started_by, None);
        assert_eq!(ids(&from_parent.started), ["started"]);
        assert_eq!(ids(&from_parent.sent), ["existing"]);
        assert!(from_parent.received.is_empty());

        let from_started = lineage(&access, &scope("started")).await.unwrap();
        assert_eq!(
            from_started.started_by.map(|link| link.scope.session_id),
            Some("parent".to_owned())
        );
        assert_eq!(ids(&from_started.received), ["other"]);

        let from_existing = lineage(&access, &scope("existing")).await.unwrap();
        assert_eq!(from_existing.started_by, None);
        assert_eq!(ids(&from_existing.received), ["parent"]);
        node.shutdown().await;
    }
}
