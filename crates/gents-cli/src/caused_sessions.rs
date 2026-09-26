use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::session::load_latest_request_in_txn;
pub(crate) use gents::session_origin::SessionScope;
use gents::session_origin::{
    load_caused_session_origins, load_request_scope, load_session_origins,
    load_session_request_doc_ids, OriginReader,
};
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

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

const HEAD_FIELDS: &str = "_docID request_id agent_did session_id requester_did behavior_id \
     content lifecycle_state superseded_by_request failure_reason created_at subagent_depth \
     caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id \
     caused_by_parent_tool_call_doc_id";

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
        load_session_origins(OriginReader::Node(node), &[cause_doc_id], "content").await?;
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
    let reader = OriginReader::Node(node);
    let mut visited = roots.iter().cloned().collect::<HashSet<_>>();
    let mut placement = roots
        .iter()
        .map(|scope| (scope.clone(), (scope.session_id.clone(), 0_u32)))
        .collect::<HashMap<_, _>>();
    let mut frontier = roots.to_vec();
    let mut caused = Vec::new();

    while !frontier.is_empty() {
        let by_doc_id = load_session_request_doc_ids(reader, &frontier)
            .await?
            .into_iter()
            .collect::<BTreeMap<_, _>>();
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
            let Some(scope) = SessionScope::of_row(&origin) else {
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
    let reader = OriginReader::Node(node);
    let mut chain = Vec::<(Value, SessionScope)>::new();
    let mut seen = HashSet::new();
    let mut label = session_id.to_string();
    loop {
        let mut origins = load_caused_session_origins(reader, &label, "content").await?;
        anyhow::ensure!(
            origins.len() <= 1,
            "ambiguous caused session label across canonical scopes: {label}"
        );
        let Some(origin) = origins.pop() else {
            return Ok(None);
        };
        let scope = SessionScope::of_row(&origin).context("caused origin omitted its scope")?;
        if !seen.insert(scope) {
            return Ok(None);
        }
        let cause_doc_id = origin
            .get("caused_by_parent_request_doc_id")
            .and_then(Value::as_str)
            .context("caused origin omitted its cause")?;
        let Some(cause_scope) = load_request_scope(reader, cause_doc_id).await? else {
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
    let mut heads = OriginReader::Node(node)
        .data(&query, "caused session head")
        .await?
        .get("AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(heads.len() <= 1, "ambiguous caused session head");
    heads
        .pop()
        .map(|row| serde_json::from_value(row).context("decoding caused session head"))
        .transpose()
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
