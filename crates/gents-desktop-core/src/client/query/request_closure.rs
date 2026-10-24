use super::snapshot_loaders::complete_request_rows;
use super::{
    execute_access_graphql_query, execute_local_graphql_query, ClientQueryError, Result,
    AGENT_REQUEST_FIELDS, AGENT_REQUEST_NAME,
};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::escape_graphql_string;
use gents_protocol::row::AgentRequestRow;
use rapidhash::{HashSetExt, RapidHashSet};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

const PHYSICAL_BATCH_SIZE: usize = 128;
const EDGE_BATCH_SIZE: usize = 64;
const ROOT_BATCH_SIZE: usize = 32;

type QueryFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Scope {
    node_did: String,
    session_id: String,
    requester_did: Option<String>,
}

impl Scope {
    fn from_row(row: &AgentRequestRow) -> Option<Self> {
        Some(Self {
            node_did: row
                .node_did
                .as_ref()
                .filter(|id| !id.trim().is_empty())?
                .clone(),
            session_id: row
                .session_id
                .as_ref()
                .filter(|id| !id.trim().is_empty())?
                .clone(),
            requester_did: row.requester_did.clone(),
        })
    }

    fn filter(&self) -> String {
        let node = escape_graphql_string(&self.node_did);
        let session = escape_graphql_string(&self.session_id);
        let requester = self
            .requester_did
            .as_deref()
            .map(|did| format!("\"{}\"", escape_graphql_string(did)))
            .unwrap_or_else(|| "null".to_string());
        format!(
            "purpose: {{ _eq: \"normal\" }}, node_did: {{ _eq: \"{node}\" }}, session_id: {{ _eq: \"{session}\" }}, requester_did: {{ _eq: {requester} }}"
        )
    }

    fn owns(&self, row: &AgentRequestRow) -> bool {
        gents_protocol::request_admission::RequestPurpose::is_public(row.purpose)
            && row.node_did.as_deref() == Some(self.node_did.as_str())
            && row.session_id.as_deref() == Some(self.session_id.as_str())
            && row.requester_did == self.requester_did
    }
}

pub(crate) async fn close_request_rows(
    node: &EmbeddedNode,
    seeds: Vec<AgentRequestRow>,
) -> Result<Vec<AgentRequestRow>> {
    close_request_rows_with(seeds, |query| {
        Box::pin(async move {
            execute_local_graphql_query(node, &query, "request dependency closure").await
        })
    })
    .await
}

pub(crate) async fn close_request_rows_on(
    access: &gents::config_client::ConfigAccess,
    seeds: Vec<AgentRequestRow>,
) -> Result<Vec<AgentRequestRow>> {
    close_request_rows_with(seeds, |query| {
        Box::pin(async move {
            execute_access_graphql_query(access, &query, "request dependency closure").await
        })
    })
    .await
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Edge {
    Physical,
    Predecessor,
    Retry,
}

struct ReadBatch {
    scope: Scope,
    edge: Edge,
    ids: Vec<String>,
}

impl ReadBatch {
    fn selection(&self, index: usize) -> String {
        let scope = self.scope.filter();
        let ids = string_list(&self.ids);
        let args = match self.edge {
            Edge::Physical => format!("docIDs: [{ids}], filter: {{ {scope} }}"),
            Edge::Predecessor => format!("filter: {{ {scope}, request_id: {{ _in: [{ids}] }} }}"),
            Edge::Retry => {
                format!("filter: {{ {scope}, retry_parent_request: {{ _in: [{ids}] }} }}")
            }
        };
        format!("r{index}: {AGENT_REQUEST_NAME}({args}) {{ {AGENT_REQUEST_FIELDS} }}")
    }
}

pub(super) async fn close_request_rows_with<'a>(
    seeds: Vec<AgentRequestRow>,
    mut execute: impl FnMut(String) -> QueryFuture<'a> + 'a,
) -> Result<Vec<AgentRequestRow>> {
    let mut rows = Vec::with_capacity(seeds.len());
    let mut frontier = Vec::new();
    let mut seen = RapidHashSet::with_capacity(seeds.len());
    let mut queried = RapidHashSet::new();

    for row in seeds {
        if let Some(scope) = Scope::from_row(&row) {
            if let Some(doc_id) = row.doc_id.as_deref() {
                if !seen.insert((scope, doc_id.to_owned())) {
                    continue;
                }
            }
            frontier.push(rows.len());
        }
        rows.push(row);
    }

    while !frontier.is_empty() {
        let mut needed: BTreeMap<(Scope, Edge), BTreeSet<String>> = BTreeMap::new();
        for index in frontier.drain(..) {
            let row = &rows[index];
            let Some(scope) = Scope::from_row(row) else {
                continue;
            };
            let folded = gents::lifecycle::folded_into(row).is_some();
            let predecessor = matches!(row.lifecycle_state, Some(
                gents_protocol::request_lifecycle::RequestLifecycleState::Pending
                | gents_protocol::request_lifecycle::RequestLifecycleState::WorkspaceBindingPending
            ))
            .then(|| {
                row.input
                    .as_ref()?
                    .queue
                    .as_ref()?
                    .queued_after_request_id
                    .as_deref()
            })
            .flatten();
            let retry = (!folded && row.lifecycle_state.is_some_and(|state| state.is_terminal()))
                .then_some(row.request_id.as_str());
            let edges = [
                (
                    Edge::Physical,
                    if folded {
                        row.superseded_by_request_doc_id.as_deref()
                    } else {
                        None
                    },
                ),
                (Edge::Physical, row.retry_parent_request_doc_id.as_deref()),
                (Edge::Predecessor, if !folded { predecessor } else { None }),
                (Edge::Retry, retry),
            ];
            for (edge, id) in edges {
                let Some(id) = id else { continue };
                if edge == Edge::Physical && seen.contains(&(scope.clone(), id.to_owned())) {
                    continue;
                }
                if queried.insert((scope.clone(), edge, id.to_owned())) {
                    needed
                        .entry((scope.clone(), edge))
                        .or_default()
                        .insert(id.to_owned());
                }
            }
        }
        let batches = needed
            .into_iter()
            .flat_map(|((scope, edge), ids)| {
                let size = if edge == Edge::Physical {
                    PHYSICAL_BATCH_SIZE
                } else {
                    EDGE_BATCH_SIZE
                };
                let ids = ids.into_iter().collect::<Vec<_>>();
                ids.chunks(size)
                    .map(|ids| ReadBatch {
                        scope: scope.clone(),
                        edge,
                        ids: ids.to_vec(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for batch in batches.chunks(ROOT_BATCH_SIZE) {
            let selections = batch
                .iter()
                .enumerate()
                .map(|(index, read)| read.selection(index))
                .collect::<Vec<_>>()
                .join("\n");
            let data = execute(format!("query RequestDependencies {{ {selections} }}")).await?;
            for (index, read) in batch.iter().enumerate() {
                let candidates = complete_request_rows(&data, &format!("r{index}"))?;
                for row in candidates {
                    if !read.scope.owns(&row) {
                        return Err(ClientQueryError::RequestDependencyScope);
                    }
                    let doc_id = row
                        .doc_id
                        .as_ref()
                        .ok_or(ClientQueryError::RequestDependencyIdentity)?;
                    if seen.insert((read.scope.clone(), doc_id.clone())) {
                        frontier.push(rows.len());
                        rows.push(row);
                    }
                }
            }
        }
    }
    crate::client::store::sort_request_rows(&mut rows);
    Ok(rows)
}

fn string_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests;
