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

/// A session whose first public request was caused by a request in a session
/// reachable from one of the loaded roots.
#[derive(Clone, Debug)]
pub(crate) struct CausedSession {
    pub(crate) scope: SessionScope,
    pub(crate) behavior_id: String,
    pub(crate) root_session_id: String,
    pub(crate) caused_by_scope: SessionScope,
    /// Distance from the root in the loaded causal walk, starting at one.
    pub(crate) depth: u32,
    pub(crate) first: AgentRequestRow,
    pub(crate) latest: AgentRequestRow,
}

const REQUEST_ROW_FIELDS: &str = r#"
    _docID
    requester_did
    request_id
    content
    session_id
    agent_did
    behavior_id
    lifecycle_state
    superseded_by_request
    failure_reason
    created_at
    subagent_depth
    caused_by_parent_request_id
    caused_by_parent_request_doc_id
    caused_by_parent_tool_call_id
    caused_by_parent_tool_call_doc_id
"#;

/// Walk the sessions caused by the given roots through the signed request
/// lineage. ACP decides which rows are visible; a session is attributed to the
/// first visible session whose request caused its earliest request, and a
/// session is visited once, so message loops terminate.
pub(crate) async fn load_caused_sessions(
    node: &EmbeddedNode,
    roots: &[SessionScope],
) -> Result<Vec<CausedSession>> {
    let mut visited = roots.iter().cloned().collect::<HashSet<_>>();
    let mut root_of = roots
        .iter()
        .map(|scope| (scope.clone(), scope.session_id.clone()))
        .collect::<HashMap<_, _>>();
    let mut depth_of = roots
        .iter()
        .map(|scope| (scope.clone(), 0_u32))
        .collect::<HashMap<_, _>>();
    let mut frontier = roots.to_vec();
    let mut caused = Vec::<CausedSession>::new();

    while !frontier.is_empty() {
        let rows = load_session_requests(node, &frontier).await?;
        let mut by_doc_id = HashMap::<String, SessionScope>::new();
        let mut by_scope = BTreeMap::<SessionScope, Vec<AgentRequestRow>>::new();
        for row in rows {
            let scope = row_scope(&row)?;
            if !frontier.contains(&scope) {
                continue;
            }
            by_doc_id.insert(
                row.doc_id
                    .clone()
                    .context("AgentRequest row omitted _docID")?,
                scope.clone(),
            );
            by_scope.entry(scope).or_default().push(row);
        }
        for session in caused
            .iter_mut()
            .filter(|session| frontier.contains(&session.scope))
        {
            let rows = by_scope
                .get(&session.scope)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if let Some(latest) = latest_request(node, &session.scope, rows).await? {
                session.latest = latest;
            }
        }
        if by_doc_id.is_empty() {
            break;
        }

        let mut doc_ids = by_doc_id.keys().cloned().collect::<Vec<_>>();
        doc_ids.sort();
        let effects = caused_requests(node, &doc_ids).await?;
        let mut next = Vec::new();
        for row in effects {
            let scope = row_scope(&row)?;
            if !visited.insert(scope.clone()) {
                continue;
            }
            let Some(cause) = row
                .caused_by_parent_request_doc_id
                .as_deref()
                .and_then(|doc_id| by_doc_id.get(doc_id))
                .cloned()
            else {
                continue;
            };
            let Some(behavior_id) = row
                .behavior_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let root_session_id = root_of[&cause].clone();
            let depth = depth_of[&cause] + 1;
            root_of.insert(scope.clone(), root_session_id.clone());
            depth_of.insert(scope.clone(), depth);
            caused.push(CausedSession {
                scope: scope.clone(),
                behavior_id,
                root_session_id,
                caused_by_scope: cause,
                depth,
                first: row.clone(),
                latest: row,
            });
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

async fn load_session_requests(
    node: &EmbeddedNode,
    scopes: &[SessionScope],
) -> Result<Vec<AgentRequestRow>> {
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
        "{{AgentRequest(filter:{{_or:[{filters}]}},order:{{created_at:ASC}}){{{REQUEST_ROW_FIELDS}}}}}"
    );
    decode_requests(node, &query, "caused sessions requests").await
}

async fn caused_requests(node: &EmbeddedNode, doc_ids: &[String]) -> Result<Vec<AgentRequestRow>> {
    let doc_ids = doc_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(",");
    let scope = public_request_filter(&format!(
        "caused_by_parent_request_doc_id: {{ _in: [{doc_ids}] }}"
    ));
    let query = format!(
        "{{AgentRequest(filter:{{{scope}}},order:{{created_at:ASC}}){{{REQUEST_ROW_FIELDS}}}}}"
    );
    decode_requests(node, &query, "caused sessions effects").await
}

async fn latest_request(
    node: &EmbeddedNode,
    scope: &SessionScope,
    rows: &[AgentRequestRow],
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
    Ok(head.and_then(|head| {
        rows.iter()
            .find(|row| row.doc_id.as_deref() == Some(head.observed.request_doc_id.as_str()))
            .cloned()
    }))
}

async fn decode_requests(
    node: &EmbeddedNode,
    query: &str,
    operation: &str,
) -> Result<Vec<AgentRequestRow>> {
    let response = graphql_with_transaction_retry(node, query, operation).await?;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding canonical AgentRequest row"))
        .collect()
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
