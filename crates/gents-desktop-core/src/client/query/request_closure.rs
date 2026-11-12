use anyhow::{Context, Result};
use futures::future::BoxFuture;
use gents_protocol::row::AgentRequestRow;
use gents_session::views::requests::{RequestOrder, RequestRows};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use rapidhash::{HashSetExt, RapidHashSet};
use std::collections::{BTreeMap, BTreeSet};

const PHYSICAL_BATCH_SIZE: usize = 128;
const EDGE_BATCH_SIZE: usize = 64;
const ROOT_BATCH_SIZE: usize = 32;

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

    fn view(&self) -> RequestRows {
        RequestRows::new(gents_session::views::transcript::Scope::Session(
            gents_protocol::session::SessionScope {
                node_did: self.node_did.clone(),
                session_id: self.session_id.clone(),
                requester_did: self.requester_did.clone(),
            },
        ))
        .public()
        .order(RequestOrder::Physical)
    }

    fn owns(&self, row: &AgentRequestRow) -> bool {
        gents_protocol::request_admission::RequestPurpose::is_public(row.purpose)
            && row.node_did.as_deref() == Some(self.node_did.as_str())
            && row.session_id.as_deref() == Some(self.session_id.as_str())
            && row.requester_did == self.requester_did
    }
}

pub(crate) async fn close_request_rows_on(
    access: &ConfigAccess,
    seeds: Vec<AgentRequestRow>,
) -> Result<Vec<AgentRequestRow>> {
    close_request_rows_with(seeds, |views| {
        Box::pin(async move { Ok(access.read_views(&views).await?) })
    })
    .await
}

pub(crate) async fn close_request_rows_in_txn(
    txn: &ConfigApplyTxn<'_>,
    seeds: Vec<AgentRequestRow>,
) -> Result<Vec<AgentRequestRow>> {
    close_request_rows_with(seeds, |views| {
        Box::pin(async move { Ok(txn.read_views(&views).await?) })
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
    fn view(&self) -> RequestRows {
        let view = self.scope.view();
        match self.edge {
            Edge::Physical => view.doc_ids(self.ids.clone()),
            Edge::Predecessor => view.request_ids(self.ids.clone()),
            Edge::Retry => view.retry_parent_ids(self.ids.clone()),
        }
    }
}

pub(super) async fn close_request_rows_with<'a>(
    seeds: Vec<AgentRequestRow>,
    mut execute: impl FnMut(Vec<RequestRows>) -> BoxFuture<'a, Result<Vec<Vec<AgentRequestRow>>>> + 'a,
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
            let folded = gents_session::request::lifecycle::folded_into(row).is_some();
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
                    if folded || gents_session::request::lifecycle::replaced_by(row).is_some() {
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
            let views = batch.iter().map(ReadBatch::view).collect::<Vec<_>>();
            let results = execute(views).await?;
            anyhow::ensure!(
                results.len() == batch.len(),
                "request dependency batch omitted results"
            );
            for (read, candidates) in batch.iter().zip(results) {
                for row in candidates {
                    anyhow::ensure!(
                        read.scope.owns(&row),
                        "request dependency query returned a row outside its exact scope"
                    );
                    let doc_id = row
                        .doc_id
                        .as_ref()
                        .context("request dependency row lacks physical identity")?;
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

#[cfg(test)]
mod tests;
