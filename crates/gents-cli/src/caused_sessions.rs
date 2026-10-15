use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::session::{load_latest_request_in_txn, public_request_filter, session_scope_filter};
pub(crate) use gents::session_origin::SessionScope;
use gents::session_origin::{lineage, load_session, started_by, SessionLink};
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct CausedSession {
    pub(crate) scope: SessionScope,
    pub(crate) agent_id: String,
    pub(crate) root_session_id: String,
    pub(crate) caused_by_scope: SessionScope,
    pub(crate) depth: u32,
    pub(crate) first: AgentRequestRow,
    pub(crate) latest: AgentRequestRow,
}

const REQUEST_FIELDS: &str = "_docID request_id node_did session_id requester_did agent_id \
     content lifecycle_state superseded_by_request failure_reason created_at request_hop \
     caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id \
     caused_by_parent_tool_call_doc_id";

fn access(node: &Arc<EmbeddedNode>) -> ConfigAccess {
    ConfigAccess::local(node.clone())
}

#[cfg(unix)]
pub(crate) async fn load_direct_caused_sessions(
    node: &Arc<EmbeddedNode>,
    cause: &AgentRequestRow,
) -> Result<Vec<CausedSession>> {
    let parent = row_scope(cause)?;
    let mut caused = Vec::new();
    for (link, first) in started_by_request(&access(node), cause).await? {
        let latest = load_session_head(node, &link.scope)
            .await?
            .unwrap_or_else(|| first.clone());
        let Some(agent_id) = first.agent_id.clone() else {
            continue;
        };
        caused.push(CausedSession {
            scope: link.scope,
            agent_id,
            root_session_id: parent.session_id.clone(),
            caused_by_scope: parent.clone(),
            depth: 1,
            first,
            latest,
        });
    }
    Ok(caused)
}

pub(crate) async fn load_caused_sessions(
    node: &Arc<EmbeddedNode>,
    roots: &[SessionScope],
) -> Result<Vec<CausedSession>> {
    let access = access(node);
    let mut visited = roots.iter().cloned().collect::<HashSet<_>>();
    let mut frontier = roots
        .iter()
        .map(|scope| (scope.clone(), scope.session_id.clone(), 0_u32))
        .collect::<Vec<_>>();
    let mut caused = Vec::new();
    while let Some((scope, root, depth)) = frontier.pop() {
        for link in lineage(&access, &scope).await?.started {
            if !visited.insert(link.scope.clone()) {
                continue;
            }
            let child = link.scope.clone();
            if let Some(session) = view(node, link, scope.clone(), root.clone(), depth + 1).await? {
                caused.push(session);
                frontier.push((child, root.clone(), depth + 1));
            }
        }
    }
    caused.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.first.created_at.cmp(&right.first.created_at))
            .then_with(|| left.scope.cmp(&right.scope))
    });
    Ok(caused)
}

pub(crate) async fn load_caused_session(
    node: &Arc<EmbeddedNode>,
    session_id: &str,
    is_root: impl Fn(&SessionScope) -> bool,
) -> Result<Option<CausedSession>> {
    let access = access(node);
    let Some(session) = load_session(&access, session_id, None).await? else {
        return Ok(None);
    };
    let mut scope = SessionScope::from(&session);
    let mut chain = Vec::<SessionLink>::new();
    let mut seen = HashSet::from([scope.clone()]);
    loop {
        let Some(link) = started_by(&access, &scope).await? else {
            return Ok(None);
        };
        if !seen.insert(link.scope.clone()) {
            return Ok(None);
        }
        let reached_root = is_root(&link.scope);
        scope = link.scope.clone();
        chain.push(link);
        if reached_root {
            break;
        }
    }
    let depth = u32::try_from(chain.len()).context("caused session chain is too deep")?;
    let root = chain
        .last()
        .map(|link| link.scope.session_id.clone())
        .context("caused session chain is empty")?;
    let parent = chain.swap_remove(0);
    let started = SessionLink {
        scope: SessionScope::from(&session),
        cause_request_doc_id: parent.cause_request_doc_id,
    };
    view(node, started, parent.scope, root, depth).await
}

/// The public request that started a linked session: the earliest request of
/// `link.scope` that names the link's cause and a tool call, ordered by
/// `(created_at, _docID)`. It is never found by tool name, so a call of any
/// kind may open it, and a later message from the same cause into the same
/// session does not displace it.
pub(crate) async fn starting_request(
    access: &ConfigAccess,
    link: &SessionLink,
) -> Result<Option<AgentRequestRow>> {
    let filter = public_request_filter(&format!(
        r#"{}, caused_by_parent_request_doc_id: {{ _eq: "{}" }}"#,
        session_scope_filter(
            &link.scope.node_did,
            &link.scope.session_id,
            link.scope.requester_did.as_deref(),
        ),
        escape_graphql_string(&link.cause_request_doc_id)
    ));
    let rows = access
        .execute(&format!(
            "{{AgentRequest(filter: {{{filter}}}) {{{REQUEST_FIELDS}}}}}"
        ))
        .await?
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    opening_request(&rows, link)
        .map(|row| serde_json::from_value(row).context("decoding AgentRequest row"))
        .transpose()
}

/// The opening request among request rows (`_docID`, `created_at`, the
/// session scope and both causal edges): the earliest in `link.scope`, by
/// `(created_at, _docID)`, whose `caused_by_parent_request_doc_id` is the
/// link's cause and whose `caused_by_parent_tool_call_doc_id` is non-null.
pub(crate) fn opening_request(rows: &[Value], link: &SessionLink) -> Option<Value> {
    let text = |row: &Value, field: &str| row.get(field).and_then(Value::as_str).map(str::to_owned);
    rows.iter()
        .filter(|row| {
            text(row, "caused_by_parent_request_doc_id").as_deref()
                == Some(&link.cause_request_doc_id)
                && text(row, "caused_by_parent_tool_call_doc_id").is_some()
                && text(row, "node_did").as_deref() == Some(&link.scope.node_did)
                && text(row, "session_id").as_deref() == Some(&link.scope.session_id)
                && text(row, "requester_did") == link.scope.requester_did
        })
        .min_by_key(|row| (text(row, "created_at"), text(row, "_docID")))
        .cloned()
}

pub(crate) async fn started_by_request(
    access: &ConfigAccess,
    cause: &AgentRequestRow,
) -> Result<Vec<(SessionLink, AgentRequestRow)>> {
    let cause_doc_id = cause
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("started sessions require a physical request")?;
    let mut started = Vec::new();
    for link in lineage(access, &row_scope(cause)?).await?.started {
        if link.cause_request_doc_id != cause_doc_id {
            continue;
        }
        if let Some(first) = starting_request(access, &link).await? {
            started.push((link, first));
        }
    }
    Ok(started)
}

async fn view(
    node: &Arc<EmbeddedNode>,
    link: SessionLink,
    caused_by_scope: SessionScope,
    root_session_id: String,
    depth: u32,
) -> Result<Option<CausedSession>> {
    let access = access(node);
    let Some(session) = load_session(
        &access,
        &link.scope.session_id,
        link.scope.requester_did.as_deref(),
    )
    .await?
    .filter(|session| SessionScope::from(session) == link.scope) else {
        return Ok(None);
    };
    let Some(first) = starting_request(&access, &link).await? else {
        return Ok(None);
    };
    let latest = load_session_head(node, &link.scope)
        .await?
        .unwrap_or_else(|| first.clone());
    Ok(Some(CausedSession {
        scope: link.scope,
        agent_id: session.agent_id,
        root_session_id,
        caused_by_scope,
        depth,
        first,
        latest,
    }))
}

pub(crate) async fn load_session_head(
    node: &Arc<EmbeddedNode>,
    scope: &SessionScope,
) -> Result<Option<AgentRequestRow>> {
    let head = ConfigAccess::transact_local_write(node, None, "caused_sessions.head", |txn| {
        Box::pin(async move {
            load_latest_request_in_txn(
                txn,
                &scope.node_did,
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
    let query = format!(
        r#"{{AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {REQUEST_FIELDS} }}}}"#,
        escape_graphql_string(&head.observed.request_doc_id)
    );
    let mut heads = request_rows(&access(node), &query).await?;
    anyhow::ensure!(heads.len() <= 1, "ambiguous caused session head");
    Ok(heads.pop())
}

async fn request_rows(access: &ConfigAccess, query: &str) -> Result<Vec<AgentRequestRow>> {
    access
        .execute(query)
        .await?
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding AgentRequest row"))
        .collect()
}

fn row_scope(row: &AgentRequestRow) -> Result<SessionScope> {
    Ok(SessionScope {
        node_did: row
            .node_did
            .clone()
            .context("AgentRequest row omitted node_did")?,
        session_id: row
            .session_id
            .clone()
            .context("AgentRequest row omitted session_id")?,
        requester_did: row.requester_did.clone(),
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// A parent request in `parent_session` and two child sessions it started,
    /// each opened by a request naming the same tool call (of no session-message
    /// kind). Returns the parent request's `_docID`.
    pub(crate) async fn seed_one_call_two_sessions(
        node: &gents::defra_node::EmbeddedNode,
        did: &str,
        parent_session: &str,
    ) -> String {
        async fn run(
            node: &gents::defra_node::EmbeddedNode,
            mutation: String,
            collection: &str,
        ) -> String {
            let response = node.execute(&mutation).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            gents::graphql::single_mutation_document(&response, &format!("create_{collection}"))
                .unwrap()
                .and_then(|document| document["_docID"].as_str().map(str::to_owned))
                .expect("created doc id")
        }
        let request = |request_id: &str, session_id: &str, cause: &str| {
            format!(
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "{did}", requester_did: "{did}", agent_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "2026-09-26T00:00:01Z", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            )
        };
        let parent = run(
            node,
            request("parent-request", parent_session, ""),
            "AgentRequest",
        )
        .await;
        for child in ["child-a", "child-b"] {
            let cause = format!(
                r#"caused_by_parent_request_id: "parent-request", caused_by_parent_request_doc_id: "{parent}", caused_by_parent_tool_call_id: "call-1", caused_by_parent_tool_call_doc_id: "call-doc","#
            );
            run(
                node,
                request(&format!("{child}-request"), child, &cause),
                "AgentRequest",
            )
            .await;
            run(
                node,
                format!(
                    r#"mutation {{ create_AgentSession(input: {{ session_id: "{child}", node_did: "{did}", requester_did: "{did}", agent_id: "worker", created_at: "2026-09-26T00:00:00Z", provenance: {{ parent_request_doc_id: "{parent}" }} }}) {{ _docID }} }}"#
                ),
                "AgentSession",
            )
            .await;
        }
        parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link() -> SessionLink {
        SessionLink {
            scope: SessionScope {
                node_did: "did:test:node".into(),
                session_id: "child".into(),
                requester_did: Some("did:test:node".into()),
            },
            cause_request_doc_id: "bae-parent".into(),
        }
    }

    fn row(doc_id: &str, created_at: &str, call: Option<&str>) -> Value {
        json!({
            "_docID": doc_id,
            "node_did": "did:test:node",
            "session_id": "child",
            "requester_did": "did:test:node",
            "created_at": created_at,
            "caused_by_parent_request_doc_id": "bae-parent",
            "caused_by_parent_tool_call_doc_id": call,
        })
    }

    fn doc_id(row: Option<Value>) -> Option<String> {
        row.map(|row| row["_docID"].as_str().unwrap().to_owned())
    }

    #[test]
    fn opening_request_prefers_agent_new_over_a_later_message() {
        let rows = [
            row("bae-message", "2026-09-26T00:00:02Z", Some("call-message")),
            row("bae-new", "2026-09-26T00:00:01Z", Some("call-new")),
        ];
        assert_eq!(
            doc_id(opening_request(&rows, &link())).as_deref(),
            Some("bae-new")
        );
    }

    #[test]
    fn opening_request_is_the_earliest_of_one_call() {
        let rows = [
            row("bae-b", "2026-09-26T00:00:01Z", Some("call-1")),
            row("bae-c", "2026-09-26T00:00:02Z", Some("call-1")),
            row("bae-a", "2026-09-26T00:00:01Z", Some("call-1")),
        ];
        assert_eq!(
            doc_id(opening_request(&rows, &link())).as_deref(),
            Some("bae-a")
        );
    }

    #[test]
    fn opening_request_needs_the_cause_the_call_edge_and_the_session() {
        let mut other_cause = row("bae-cause", "2026-09-26T00:00:00Z", Some("call-1"));
        other_cause["caused_by_parent_request_doc_id"] = json!("bae-other");
        let mut other_session = row("bae-session", "2026-09-26T00:00:00Z", Some("call-1"));
        other_session["session_id"] = json!("elsewhere");
        let rows = [
            other_cause,
            other_session,
            row("bae-no-call", "2026-09-26T00:00:00Z", None),
        ];
        assert_eq!(opening_request(&rows, &link()), None);
    }
}
