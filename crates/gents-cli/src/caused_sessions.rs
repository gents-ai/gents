//! Shim views of the sessions a session started. Lineage itself is read only
//! through `gents::session_origin`; this module maps its links onto the
//! request rows the shims present.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::session::{load_latest_request_in_txn, public_request_filter, session_scope_filter};
pub(crate) use gents::session_origin::SessionScope;
use gents::session_origin::{caused_requests, lineage, load_session, started_by, SessionLink};
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

/// A session another session's request started.
#[derive(Clone, Debug)]
pub(crate) struct CausedSession {
    pub(crate) scope: SessionScope,
    pub(crate) agent_id: String,
    pub(crate) root_session_id: String,
    pub(crate) caused_by_scope: SessionScope,
    /// Distance from the root of the resolved chain, starting at one.
    pub(crate) depth: u32,
    /// The request that started the session, carrying the causing edge.
    pub(crate) first: AgentRequestRow,
    pub(crate) latest: AgentRequestRow,
}

const REQUEST_FIELDS: &str = "_docID request_id node_did session_id requester_did agent_id \
     content lifecycle_state superseded_by_request failure_reason created_at request_hop \
     caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id \
     caused_by_parent_tool_call_doc_id";

fn access(node: &Arc<EmbeddedNode>) -> ConfigAccess {
    ConfigAccess::Local(node.clone())
}

/// Sessions started by this exact physical request.
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

/// Every session transitively started by the given roots. Each session has
/// one starter, so it is visited once and message loops terminate. Use
/// [`load_direct_caused_sessions`] or [`load_caused_session`] where one level
/// or one session is enough.
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

/// Resolve one session label to the session it names by following its
/// starters until `is_root` accepts one. `None` when the label names no
/// started session or no accepted root is among its starters.
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

/// The public request that started a linked session. The causing request's
/// `agent_new` calls name the requests they caused
/// (`session_origin::caused_requests`); the one of those in the linked
/// session is the opening request. A later message into the same session
/// from the same request is caused by an `agent_message` call instead.
pub(crate) async fn starting_request(
    access: &ConfigAccess,
    link: &SessionLink,
) -> Result<Option<AgentRequestRow>> {
    let calls = access
        .execute(&format!(
            r#"{{AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }} }}) {{ _docID }}}}"#,
            escape_graphql_string(&link.cause_request_doc_id),
            escape_graphql_string(gents::toolset::AGENT_NEW_TOOL_NAME),
        ))
        .await?;
    let call_doc_ids = calls
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("_docID")?.as_str())
        .collect::<Vec<_>>();
    let caused = caused_requests(access, call_doc_ids.iter().copied()).await?;
    if caused.is_empty() {
        return Ok(None);
    }
    let filter = public_request_filter(&format!(
        "{}, request_id: {{ _in: [{}] }}",
        session_scope_filter(
            &link.scope.node_did,
            &link.scope.session_id,
            link.scope.requester_did.as_deref(),
        ),
        caused
            .values()
            .map(|id| format!("\"{}\"", escape_graphql_string(id)))
            .collect::<Vec<_>>()
            .join(",")
    ));
    let mut rows = request_rows(
        access,
        &format!("{{AgentRequest(filter: {{{filter}}}) {{{REQUEST_FIELDS}}}}}"),
    )
    .await?;
    rows.retain(|row| {
        row.caused_by_parent_request_doc_id.as_deref() == Some(&link.cause_request_doc_id)
    });
    anyhow::ensure!(
        rows.len() <= 1,
        "session {} has more than one opening request",
        link.scope.session_id
    );
    Ok(rows.pop())
}

/// The sessions this exact request started, with each one's starting request.
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

/// Present one started session: its stored agent, the request that
/// started it and its current head.
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

/// The latest request of one session, selected by the canonical head owner.
pub(crate) async fn load_session_head(
    node: &Arc<EmbeddedNode>,
    scope: &SessionScope,
) -> Result<Option<AgentRequestRow>> {
    let head = ConfigAccess::transact_local(node, None, "caused_sessions.head", |txn| {
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
    // The exact `_docID` read carries no `order`, so `limit` cannot hide it.
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
