//! Session lineage: how one session is linked to others by agent messages.
//!
//! A session was started by another request only when its stored
//! `AgentSession.provenance.parent_request_doc_id` names that request; the
//! runtime copies it from the session's first request when it creates the
//! session. A message into an existing session never rewrites it, so it never
//! makes that session a started one. Every other link is a request whose
//! `caused_by_parent_request_doc_id` names a request in another session. Every
//! lineage reader goes through [`lineage`].

use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::Value;

use crate::config_client::{ConfigAccess, ConfigRead};
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::session::{
    decode_session_row, public_request_filter, session_scope_filter, AGENT_SESSION_FIELDS,
};
use gents_protocol::session::AgentSession;

/// The exact scope of one session label.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionScope {
    pub agent_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
}

impl SessionScope {
    /// The session a request runs in.
    pub fn of_request(request: &crate::AgentRequest) -> Self {
        Self {
            agent_did: request.agent_did.clone(),
            session_id: request.session_id.clone(),
            requester_did: request.requester_did.clone(),
        }
    }

    fn of_row(row: &Value) -> Option<Self> {
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

/// Another session and the request that links it to this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLink {
    pub scope: SessionScope,
    /// The causing request: in the other session for `started_by` and
    /// `received`, in this session for `started` and `sent`.
    pub cause_request_doc_id: String,
}

/// Every agent-message link of one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionLineage {
    /// The session whose request started this one.
    pub started_by: Option<SessionLink>,
    /// Sessions this session's requests started.
    pub started: Vec<SessionLink>,
    /// Other sessions this session sent a request into without starting them.
    pub sent: Vec<SessionLink>,
    /// Other sessions, besides its starter, that sent this session a request.
    pub received: Vec<SessionLink>,
}

const SCOPE_FIELDS: &str = "_docID agent_did session_id requester_did";

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
            agent_did: session.agent_did.clone(),
            session_id: session.session_id.clone(),
            requester_did: session.requester_did.clone(),
        }
    }
}

/// The scopes of the given physical requests, keyed by document id.
pub async fn request_scopes<'a, R: ConfigRead + ?Sized>(
    access: &R,
    doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<(String, SessionScope)>> {
    let doc_ids = doc_ids.into_iter().collect::<BTreeSet<_>>();
    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let doc_ids = doc_ids.into_iter().collect::<Vec<_>>();
    let mut scopes = Vec::new();
    // Bounded predicates, without truncating lineage or ever interpolating
    // an empty list literal into a database operation.
    for batch in doc_ids.chunks(128) {
        let query = format!(
            "{{AgentRequest(filter: {{_docID: {{_in: {}}}}}) {{{SCOPE_FIELDS}}}}}",
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

/// The session stored under `session_id` for `requester_did` (any requester
/// when `None`), when exactly one is visible.
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

/// The session that started `scope`, from its stored provenance.
pub async fn started_by(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> Result<Option<SessionLink>> {
    let own = sessions(
        access,
        &session_scope_filter(
            &scope.agent_did,
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

/// The request each physical tool call caused: the one public request whose
/// `caused_by_parent_tool_call_doc_id` names that call. A call named by more
/// than one request has no single caused request and is omitted.
pub async fn caused_requests<'a>(
    access: &ConfigAccess,
    tool_call_doc_ids: impl IntoIterator<Item = &'a str>,
) -> Result<std::collections::BTreeMap<String, String>> {
    let tool_call_doc_ids = tool_call_doc_ids.into_iter().collect::<BTreeSet<_>>();
    if tool_call_doc_ids.is_empty() {
        return Ok(Default::default());
    }
    let query = format!(
        "{{AgentRequest(filter: {{{}}}) {{request_id caused_by_parent_tool_call_doc_id}}}}",
        public_request_filter(&format!(
            "caused_by_parent_tool_call_doc_id: {{ _in: {} }}",
            graphql_string_list_literal(tool_call_doc_ids)
        ))
    );
    let mut caused = std::collections::BTreeMap::<String, Option<String>>::new();
    for row in collection(access, &query, "AgentRequest").await? {
        let (Some(tool), Some(request_id)) = (
            row.get("caused_by_parent_tool_call_doc_id")
                .and_then(Value::as_str),
            row.get("request_id").and_then(Value::as_str),
        ) else {
            continue;
        };
        caused
            .entry(tool.to_owned())
            .and_modify(|single| *single = None)
            .or_insert_with(|| Some(request_id.to_owned()));
    }
    Ok(caused
        .into_iter()
        .filter_map(|(tool, request_id)| Some((tool, request_id?)))
        .collect())
}

/// Every agent-message link of `scope`.
pub async fn lineage(access: &ConfigAccess, scope: &SessionScope) -> Result<SessionLineage> {
    let own_filter = session_scope_filter(
        &scope.agent_did,
        &scope.session_id,
        scope.requester_did.as_deref(),
    );
    let started_by = started_by(access, scope).await?;

    // Every public request of this session, with the edge it carries.
    let own_requests = collection(
        access,
        &format!(
            "{{AgentRequest(filter: {{{}}}) {{{SCOPE_FIELDS} caused_by_parent_request_doc_id}}}}",
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
    let mut received = Vec::new();
    for (cause, cause_scope) in request_scopes(access, causes).await? {
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
            ..Default::default()
        });
    }

    // A session this session started names one of its requests as provenance;
    // its requester is this session's principal.
    let mut started = Vec::new();
    for session in sessions(
        access,
        &format!(
            r#"requester_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(&scope.agent_did)
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

    // Requests in other sessions caused by this session's requests.
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
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", agent_did: "did:test:agent", requester_did: "did:test:agent", behavior_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{created_at}", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            ),
            "AgentRequest",
        )
        .await
    }

    async fn seed_session(node: &EmbeddedNode, session_id: &str, parent: Option<&str>) {
        let session = AgentSession {
            session_id: session_id.into(),
            agent_did: "did:test:agent".into(),
            requester_did: Some("did:test:agent".into()),
            behavior_id: "worker".into(),
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

    fn scope(session_id: &str) -> SessionScope {
        SessionScope {
            agent_did: "did:test:agent".into(),
            session_id: session_id.into(),
            requester_did: Some("did:test:agent".into()),
        }
    }

    fn ids(links: &[SessionLink]) -> Vec<&str> {
        links
            .iter()
            .map(|link| link.scope.session_id.as_str())
            .collect()
    }

    /// Only the stored provenance makes a session started: a message into an
    /// existing session, even one ordered first in the same second, is a
    /// `sent`/`received` link, never a start.
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
        // A later message from another session in the same second, ordered
        // first by its request id, takes no authority over the session.
        let other = seed(&node, "o1", "other", "2026-09-26T00:00:01Z", None).await;
        seed_session(&node, "other", None).await;
        seed(&node, "a0", "started", "2026-09-26T00:00:02Z", Some(&other)).await;
        let access = ConfigAccess::Local(node.clone());

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
