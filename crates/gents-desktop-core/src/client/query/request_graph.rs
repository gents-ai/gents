use std::collections::{BTreeMap, BTreeSet};

use gents::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::*;

pub(super) async fn load_local(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
    node_did: Option<&str>,
) -> Result<ClientStore> {
    ConfigAccess::transact_local_readonly(node, None, "desktop.request_graph", |txn| {
        Box::pin(load(txn, request_id, node_did))
    })
    .await
}

pub(super) async fn load_on(
    access: &ConfigAccess,
    request_id: &str,
    node_did: Option<&str>,
) -> Result<ClientStore> {
    access
        .transact_readonly("desktop.request_graph", |txn| {
            Box::pin(load(txn, request_id, node_did))
        })
        .await
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Edge {
    Physical(String),
    Logical(String),
    Retry(String),
    Folded(String),
}

impl Edge {
    fn normalized(self) -> Option<Self> {
        let clean = |value: String| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        match self {
            Self::Physical(value) => clean(value).map(Self::Physical),
            Self::Logical(value) => clean(value).map(Self::Logical),
            Self::Retry(value) => clean(value).map(Self::Retry),
            Self::Folded(value) => clean(value).map(Self::Folded),
        }
    }

    fn selection(&self, scope: &str, index: usize) -> String {
        let (field, value) = match self {
            Self::Physical(id) => ("_docID", id),
            Self::Logical(id) => ("request_id", id),
            Self::Retry(id) => ("retry_parent_request", id),
            Self::Folded(id) => ("superseded_by_request_doc_id", id),
        };
        let folded = if matches!(self, Self::Folded(_)) {
            format!(
                ", failure_reason: {{_eq: \"{}\"}}",
                escape_graphql_string(gents::lifecycle::FOLDED_REASON)
            )
        } else {
            String::new()
        };
        format!("edge{index}: AgentRequest(filter: {{{scope}, {field}: {{_eq: \"{}\"}}{folded}}}) {{ {AGENT_REQUEST_FIELDS} }}", escape_graphql_string(value))
    }
}

/// Request links share one native snapshot and session/requester scope.
/// A response cannot supply dependency facts from another scope.
async fn load(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
    node_did: Option<&str>,
) -> Result<ClientStore> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Ok(ClientStore::default());
    }
    let node_filter = node_did
        .map(|did| format!("node_did: {{_eq: \"{}\"}},", escape_graphql_string(did)))
        .unwrap_or_default();
    let lookup = format!("query DesktopLocalRequestLookup {{ AgentRequest(filter: {{{node_filter} request_id: {{_eq: \"{}\"}}}}, limit: 1) {{ {AGENT_REQUEST_FIELDS} }} }}", escape_graphql_string(request_id));
    let data = txn.execute(&lookup).await?;
    let selected: Vec<AgentRequestRow> =
        serde_json::from_value(data["data"][AGENT_REQUEST_NAME].clone())?;
    let Some(row) = selected.first() else {
        return Ok(ClientStore::default());
    };
    anyhow::ensure!(
        row.request_id == request_id
            && node_did.is_none_or(|node| row.node_did.as_deref() == Some(node)),
        "request lookup returned a different request scope"
    );
    let expected_node = row.node_did.clone();
    let expected_session = row.session_id.clone();
    let expected_requester = row.requester_did.clone();
    let (Some(node), Some(session)) = (row.node_did.as_deref(), row.session_id.as_deref()) else {
        return Ok(ClientStore::from_rows(ClientStoreRows {
            requests: selected,
            ..Default::default()
        }));
    };
    let requester = row
        .requester_did
        .as_deref()
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let node = escape_graphql_string(node);
    let session = escape_graphql_string(session);
    let scope = format!("node_did: {{_eq: \"{node}\"}}, session_id: {{_eq: \"{session}\"}}, requester_did: {{_eq: {requester}}}");
    let states = RequestLifecycleState::nonterminal_graphql_list();
    let query = format!(
        r#"query DesktopRemoteChatPatch {{
        AgentRequest(filter: {{{scope}, lifecycle_state: {{_in: {states}}}}}) {{ {AGENT_REQUEST_FIELDS} }}
        AgentSession(filter: {{{scope}}}) {{ {AGENT_SESSION_FIELDS} }}
        Goal(filter: {{node_did: {{_eq: "{node}"}}, session_id: {{_eq: "{session}"}}}}) {{ {GOAL_FIELDS} }}
    }}"#
    );
    let data = txn.execute(&query).await?;
    let mut patch = ClientStoreRows {
        requests: serde_json::from_value(data["data"][AGENT_REQUEST_NAME].clone())?,
        sessions: parse_query_rows(&data["data"], AGENT_SESSION_NAME)?,
        goals: parse_query_rows(&data["data"], GOAL_NAME)?,
        ..Default::default()
    };
    let mut pending = selected;
    pending.append(&mut patch.requests);
    let mut rows = BTreeMap::new();
    let mut visited = BTreeSet::new();
    while !pending.is_empty() {
        let mut edges = BTreeSet::new();
        for row in pending.drain(..) {
            anyhow::ensure!(
                row.node_did == expected_node
                    && row.session_id == expected_session
                    && row.requester_did == expected_requester,
                "request dependency crossed the selected session scope"
            );
            let doc = row
                .doc_id
                .clone()
                .context("request graph row lacks physical identity")?;
            if rows.contains_key(&doc) {
                continue;
            }
            edges.insert(Edge::Folded(doc.clone()));
            if gents::lifecycle::folded_into(&row).is_some() {
                if let Some(owner) = &row.superseded_by_request_doc_id {
                    edges.insert(Edge::Physical(owner.clone()));
                }
            } else if row
                .lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal)
            {
                edges.insert(Edge::Retry(row.request_id.clone()));
            } else if matches!(
                row.lifecycle_state,
                Some(
                    RequestLifecycleState::Pending | RequestLifecycleState::WorkspaceBindingPending
                )
            ) {
                if let Some(ahead) = row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .and_then(|queue| queue.queued_after_request_id.as_ref())
                {
                    edges.insert(Edge::Logical(ahead.clone()));
                }
            }
            if let Some(parent) = &row.retry_parent_request {
                edges.insert(Edge::Logical(parent.clone()));
            }
            if let Some(parent) = &row.retry_parent_request_doc_id {
                edges.insert(Edge::Physical(parent.clone()));
            }
            rows.insert(doc, row);
        }
        let edges = edges
            .into_iter()
            .filter_map(Edge::normalized)
            .filter(|edge| visited.insert(edge.clone()))
            .collect::<Vec<_>>();
        for chunk in edges.chunks(32) {
            let selections = chunk
                .iter()
                .enumerate()
                .map(|(index, edge)| edge.selection(&scope, index))
                .collect::<Vec<_>>()
                .join("\n");
            let data = txn
                .execute(&format!(
                    "query DesktopRequestDependencies {{ {selections} }}"
                ))
                .await?;
            for i in 0..chunk.len() {
                pending.extend(serde_json::from_value::<Vec<AgentRequestRow>>(
                    data["data"][format!("edge{i}")].clone(),
                )?);
            }
        }
    }
    patch.requests = rows.into_values().collect();
    Ok(ClientStore::from_rows(patch))
}
