use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents::session::{load_latest_request_in_txn, public_request_filter, session_scope_filter};
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

/// The canonical scope of one session label.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SessionScope {
    pub(crate) agent_did: String,
    pub(crate) session_id: String,
    pub(crate) requester_did: Option<String>,
}

/// A session whose origin (its first public request) was caused by a request
/// in another session.
#[derive(Clone, Debug)]
pub(crate) struct CausedSession {
    pub(crate) scope: SessionScope,
    pub(crate) behavior_id: String,
    pub(crate) root_session_id: String,
    pub(crate) caused_by_scope: SessionScope,
    /// Distance from the root of the resolved causal chain, starting at one.
    pub(crate) depth: u32,
    pub(crate) first: AgentRequestRow,
    pub(crate) latest: AgentRequestRow,
}

/// Where lineage reads run: the embedded node, or the HTTP GraphQL endpoint
/// through the configuration read owner.
#[derive(Clone, Copy)]
pub(crate) enum LineageReader<'a> {
    Node(&'a EmbeddedNode),
    Graphql(&'a str),
}

impl LineageReader<'_> {
    async fn data(&self, query: &str, operation: &str) -> Result<Value> {
        match self {
            Self::Node(node) => Ok(graphql_with_transaction_retry(node, query, operation)
                .await?
                .data
                .unwrap_or(Value::Null)),
            Self::Graphql(url) => Ok(ConfigAccess::Graphql((*url).to_string())
                .execute(query)
                .await?
                .get("data")
                .cloned()
                .unwrap_or(Value::Null)),
        }
    }
}

/// Fields every origin read selects: identity, scope and the causing edge.
pub(crate) const ORIGIN_FIELDS: &str = "_docID request_id agent_did session_id requester_did \
     behavior_id created_at lifecycle_state caused_by_parent_request_id \
     caused_by_parent_request_doc_id caused_by_parent_tool_call_id \
     caused_by_parent_tool_call_doc_id";

const HEAD_FIELDS: &str = "_docID request_id agent_did session_id requester_did behavior_id \
     content lifecycle_state superseded_by_request failure_reason created_at subagent_depth \
     caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id \
     caused_by_parent_tool_call_doc_id";

/// The public requests that began a session and were caused by one of
/// `cause_doc_ids`. A request appended to an existing session (a message to
/// an idle or busy session) carries the same causing edge but is not the
/// session's origin, so it never makes that session a caused session.
/// `extra_fields` are selected in addition to [`ORIGIN_FIELDS`].
pub(crate) async fn load_session_origins(
    reader: LineageReader<'_>,
    cause_doc_ids: &[String],
    extra_fields: &str,
) -> Result<Vec<Value>> {
    if cause_doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let doc_ids = graphql_string_list(cause_doc_ids);
    let filter = public_request_filter(&format!(
        "caused_by_parent_request_doc_id: {{ _in: [{doc_ids}] }}"
    ));
    let query = format!(
        "{{AgentRequest(filter:{{{filter}}},order:{{created_at:ASC}}){{{ORIGIN_FIELDS} {extra_fields}}}}}"
    );
    let rows = rows(&reader.data(&query, "caused session candidates").await?);
    retain_session_origins(reader, rows).await
}

/// The caused origins of `session_id` across scopes.
async fn load_caused_origins_of(reader: LineageReader<'_>, session_id: &str) -> Result<Vec<Value>> {
    let filter = public_request_filter(&format!(
        r#"session_id: {{ _eq: "{}" }}, caused_by_parent_request_doc_id: {{ _ne: null }}"#,
        escape_graphql_string(session_id)
    ));
    let query = format!("{{AgentRequest(filter:{{{filter}}}){{{ORIGIN_FIELDS} content}}}}");
    let rows = rows(&reader.data(&query, "caused session origin").await?);
    retain_session_origins(reader, rows).await
}

/// Keep only candidates that are the first public request of their session
/// under the canonical request order (`created_at`, then `request_id`).
async fn retain_session_origins(reader: LineageReader<'_>, rows: Vec<Value>) -> Result<Vec<Value>> {
    let mut probes = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (Some(scope), Some(created_at), Some(request_id)) = (
            value_scope(row),
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
    let data = reader.data(&query, "caused session origin check").await?;
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

/// Sessions whose origin was caused by this exact physical request.
pub(crate) async fn load_direct_caused_sessions(
    node: &EmbeddedNode,
    cause: &AgentRequestRow,
) -> Result<Vec<CausedSession>> {
    let cause_scope = row_scope(cause)?;
    let cause_doc_id = cause
        .doc_id
        .clone()
        .filter(|id| !id.is_empty())
        .context("caused sessions require a physical request")?;
    let origins =
        load_session_origins(LineageReader::Node(node), &[cause_doc_id], "content").await?;
    let mut caused = Vec::new();
    for origin in origins {
        let Some(session) = caused_session(
            origin,
            cause_scope.clone(),
            cause_scope.session_id.clone(),
            1,
        )?
        else {
            continue;
        };
        caused.push(with_head(node, session).await?);
    }
    Ok(caused)
}

/// Walk every session transitively caused by the given roots. Each session
/// has one origin, so its parent does not depend on which roots are passed,
/// and each session is visited once, so message loops terminate. Use
/// [`load_direct_caused_sessions`] or [`load_caused_session`] where one level
/// or one session is enough.
pub(crate) async fn load_caused_sessions(
    node: &EmbeddedNode,
    roots: &[SessionScope],
) -> Result<Vec<CausedSession>> {
    let reader = LineageReader::Node(node);
    let mut visited = roots.iter().cloned().collect::<HashSet<_>>();
    let mut placement = roots
        .iter()
        .map(|scope| (scope.clone(), (scope.session_id.clone(), 0_u32)))
        .collect::<HashMap<_, _>>();
    let mut frontier = roots.to_vec();
    let mut caused = Vec::new();

    while !frontier.is_empty() {
        let by_doc_id = request_doc_ids(reader, &frontier).await?;
        let doc_ids = by_doc_id.keys().cloned().collect::<Vec<_>>();
        let mut next = Vec::new();
        for origin in load_session_origins(reader, &doc_ids, "content").await? {
            let Some(cause) = origin
                .get("caused_by_parent_request_doc_id")
                .and_then(Value::as_str)
                .and_then(|doc_id| by_doc_id.get(doc_id))
                .cloned()
            else {
                continue;
            };
            let Some(scope) = value_scope(&origin) else {
                continue;
            };
            if !visited.insert(scope.clone()) {
                continue;
            }
            let (root_session_id, depth) = placement[&cause].clone();
            let Some(session) = caused_session(origin, cause, root_session_id.clone(), depth + 1)?
            else {
                continue;
            };
            placement.insert(scope.clone(), (root_session_id, depth + 1));
            caused.push(with_head(node, session).await?);
            next.push(scope);
        }
        frontier = next;
    }

    caused.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.first.created_at.cmp(&right.first.created_at))
            .then_with(|| left.scope.cmp(&right.scope))
    });
    Ok(caused)
}

/// Resolve one session label to the caused session it names by walking its
/// origins upward until `is_root` accepts an ancestor. Returns `None` when
/// the label is not caused or no accepted root is among its ancestors.
pub(crate) async fn load_caused_session(
    node: &EmbeddedNode,
    session_id: &str,
    is_root: impl Fn(&SessionScope) -> bool,
) -> Result<Option<CausedSession>> {
    let reader = LineageReader::Node(node);
    let mut chain = Vec::<(Value, SessionScope)>::new();
    let mut seen = HashSet::new();
    let mut label = session_id.to_string();
    loop {
        let mut origins = load_caused_origins_of(reader, &label).await?;
        anyhow::ensure!(
            origins.len() <= 1,
            "ambiguous caused session label across canonical scopes: {label}"
        );
        let Some(origin) = origins.pop() else {
            return Ok(None);
        };
        let scope = value_scope(&origin).context("caused origin omitted its scope")?;
        if !seen.insert(scope) {
            return Ok(None);
        }
        let cause_doc_id = origin
            .get("caused_by_parent_request_doc_id")
            .and_then(Value::as_str)
            .context("caused origin omitted its cause")?;
        // The exact `_docID` read carries no `order`, so `limit` cannot hide it.
        let query = format!(
            r#"{{AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID agent_did session_id requester_did }}}}"#,
            escape_graphql_string(cause_doc_id)
        );
        let mut causes = rows(&reader.data(&query, "caused session parent").await?);
        anyhow::ensure!(
            causes.len() <= 1,
            "ambiguous causing request {cause_doc_id}"
        );
        let Some(cause_scope) = causes.pop().as_ref().and_then(value_scope) else {
            return Ok(None);
        };
        let reached_root = is_root(&cause_scope);
        label = cause_scope.session_id.clone();
        chain.push((origin, cause_scope));
        if reached_root {
            break;
        }
    }
    let depth = u32::try_from(chain.len()).context("caused session chain is too deep")?;
    let root_session_id = chain
        .last()
        .map(|(_, root)| root.session_id.clone())
        .context("caused session chain is empty")?;
    let (origin, cause) = chain.swap_remove(0);
    let Some(session) = caused_session(origin, cause, root_session_id, depth)? else {
        return Ok(None);
    };
    Ok(Some(with_head(node, session).await?))
}

fn caused_session(
    origin: Value,
    caused_by_scope: SessionScope,
    root_session_id: String,
    depth: u32,
) -> Result<Option<CausedSession>> {
    let first: AgentRequestRow =
        serde_json::from_value(origin).context("decoding caused session origin")?;
    let scope = row_scope(&first)?;
    let Some(behavior_id) = first
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
    else {
        return Ok(None);
    };
    Ok(Some(CausedSession {
        scope,
        behavior_id,
        root_session_id,
        caused_by_scope,
        depth,
        latest: first.clone(),
        first,
    }))
}

async fn with_head(node: &EmbeddedNode, mut session: CausedSession) -> Result<CausedSession> {
    if let Some(latest) = load_session_head(node, &session.scope).await? {
        session.latest = latest;
    }
    Ok(session)
}

/// The latest request of one session, selected by the canonical head owner.
pub(crate) async fn load_session_head(
    node: &EmbeddedNode,
    scope: &SessionScope,
) -> Result<Option<AgentRequestRow>> {
    let head = ConfigAccess::transact_local(node, None, "caused_sessions.head", |txn| {
        Box::pin(async move {
            load_latest_request_in_txn(
                txn,
                &scope.agent_did,
                &scope.session_id,
                Some(scope.requester_did.as_deref()),
            )
            .await
        })
    })
    .await?;
    let Some(head) = head else {
        return Ok(None);
    };
    // The exact `_docID` read carries no `order`, so `limit` cannot hide it.
    let query = format!(
        r#"{{AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {HEAD_FIELDS} }}}}"#,
        escape_graphql_string(&head.observed.request_doc_id)
    );
    let mut heads = rows(
        &LineageReader::Node(node)
            .data(&query, "caused session head")
            .await?,
    );
    anyhow::ensure!(heads.len() <= 1, "ambiguous caused session head");
    heads
        .pop()
        .map(|row| serde_json::from_value(row).context("decoding caused session head"))
        .transpose()
}

/// Physical identities of the public requests in the given sessions.
async fn request_doc_ids(
    reader: LineageReader<'_>,
    scopes: &[SessionScope],
) -> Result<BTreeMap<String, SessionScope>> {
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
    let mut by_doc_id = BTreeMap::new();
    for row in rows(&reader.data(&query, "caused session requests").await?) {
        let (Some(doc_id), Some(scope)) =
            (row.get("_docID").and_then(Value::as_str), value_scope(&row))
        else {
            continue;
        };
        if scopes.contains(&scope) {
            by_doc_id.insert(doc_id.to_string(), scope);
        }
    }
    Ok(by_doc_id)
}

fn rows(data: &Value) -> Vec<Value> {
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

fn value_scope(row: &Value) -> Option<SessionScope> {
    Some(SessionScope {
        agent_did: row.get("agent_did")?.as_str()?.to_string(),
        session_id: row.get("session_id")?.as_str()?.to_string(),
        requester_did: row
            .get("requester_did")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

fn row_scope(row: &AgentRequestRow) -> Result<SessionScope> {
    Ok(SessionScope {
        agent_did: row
            .agent_did
            .clone()
            .context("AgentRequest row omitted agent_did")?,
        session_id: row
            .session_id
            .clone()
            .context("AgentRequest row omitted session_id")?,
        requester_did: row.requester_did.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed(
        node: &EmbeddedNode,
        request_id: &str,
        agent_did: &str,
        session_id: &str,
        created_at: &str,
        cause_doc_id: Option<&str>,
    ) -> String {
        let cause = cause_doc_id
            .map(|doc_id| {
                format!(
                    r#"caused_by_parent_request_id: "parent", caused_by_parent_request_doc_id: "{}", caused_by_parent_tool_call_id: "call-1","#,
                    escape_graphql_string(doc_id)
                )
            })
            .unwrap_or_default();
        let response = node
            .execute(&format!(
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", agent_did: "{agent_did}", requester_did: "did:test:parent", behavior_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{created_at}", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = LineageReader::Node(node)
            .data(
                &format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{request_id}"}}}}) {{_docID}}}}"#),
                "seed readback",
            )
            .await
            .unwrap();
        rows(&data)[0]["_docID"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn a_message_into_an_existing_session_does_not_make_it_caused() {
        let dir = tempfile::tempdir().unwrap();
        let node = EmbeddedNode::builder()
            .data_path(dir.path().join("node"))
            .build()
            .await
            .unwrap();
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let parent = seed(
            &node,
            "p1",
            "did:test:parent",
            "parent",
            "2026-09-26T00:00:01Z",
            None,
        )
        .await;
        seed(
            &node,
            "s1",
            "did:test:worker",
            "existing",
            "2026-09-26T00:00:00Z",
            None,
        )
        .await;
        seed(
            &node,
            "s2",
            "did:test:worker",
            "existing",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;
        seed(
            &node,
            "c1",
            "did:test:worker",
            "started",
            "2026-09-26T00:00:02Z",
            Some(&parent),
        )
        .await;

        let origins = load_session_origins(LineageReader::Node(&node), &[parent.clone()], "")
            .await
            .unwrap();
        assert_eq!(
            origins
                .iter()
                .map(|row| row["request_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["c1"]
        );

        let root = SessionScope {
            agent_did: "did:test:parent".into(),
            session_id: "parent".into(),
            requester_did: Some("did:test:parent".into()),
        };
        let walked = load_caused_sessions(&node, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(
            walked
                .iter()
                .map(|session| session.scope.session_id.as_str())
                .collect::<Vec<_>>(),
            ["started"]
        );
        assert!(load_caused_session(&node, "existing", |_| true)
            .await
            .unwrap()
            .is_none());
        let started = load_caused_session(&node, "started", |scope| scope == &root)
            .await
            .unwrap()
            .expect("caused session resolves upward");
        assert_eq!(started.caused_by_scope, root);
        assert_eq!(started.root_session_id, "parent");
        assert_eq!(started.depth, 1);
        node.shutdown().await;
    }
}
