use anyhow::{Context, Result};
use futures::future::BoxFuture;
use gents_protocol::row::AgentRequestRow;
use gents_session::views::requests::{RequestOrder, RequestRows};
use gents_store::config_client::ConfigApplyTxn;
use rapidhash::RapidHashSet;
use std::collections::{BTreeMap, BTreeSet};

const PHYSICAL_BATCH_SIZE: usize = 128;
const EDGE_BATCH_SIZE: usize = 64;
/// The most views one document carries. Each request to a Node reached
/// through a relay costs a round trip, so a round fills as few documents as
/// this allows, split evenly; every root keeps its own exact filter.
const ROOT_BATCH_SIZE: usize = 128;
/// How many dependency batches of one round are read at once.
const CLOSURE_BATCHES_AT_ONCE: usize = 4;

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

#[cfg(test)]
pub(crate) async fn close_request_rows_on(
    access: &gents_store::config_client::ConfigAccess,
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

/// Reads one document's views, each a root, answered in view order; a round
/// puts at most [`ROOT_BATCH_SIZE`] views in a document.
pub(super) type ExecuteViews<'a> =
    dyn FnMut(Vec<RequestRows>) -> BoxFuture<'a, Result<Vec<Vec<AgentRequestRow>>>> + Send + 'a;

pub(super) async fn close_request_rows_with<'a>(
    seeds: Vec<AgentRequestRow>,
    mut execute: impl FnMut(Vec<RequestRows>) -> BoxFuture<'a, Result<Vec<Vec<AgentRequestRow>>>>
        + Send
        + 'a,
) -> Result<Vec<AgentRequestRow>> {
    let mut walk = RequestClosure::default();
    walk.seed(seeds);
    while walk.round(Vec::new(), &mut execute).await?.is_some() {}
    Ok(walk.finish())
}

/// The dependency walk from seed rows to every row they need, a round at a
/// time. A caller may add seeds between rounds and send its own reads with a
/// round, so reads that do not depend on each other share a request; the
/// rows reached are the same whenever a seed arrives.
#[derive(Default)]
pub(super) struct RequestClosure {
    rows: Vec<AgentRequestRow>,
    frontier: Vec<usize>,
    seen: RapidHashSet<(Scope, String)>,
    queried: RapidHashSet<(Scope, Edge, String)>,
}

impl RequestClosure {
    pub(super) fn seed(&mut self, seeds: impl IntoIterator<Item = AgentRequestRow>) {
        for row in seeds {
            if let Some(scope) = Scope::from_row(&row) {
                if let Some(doc_id) = row.doc_id.as_deref() {
                    if !self.seen.insert((scope, doc_id.to_owned())) {
                        continue;
                    }
                }
                self.frontier.push(self.rows.len());
            }
            self.rows.push(row);
        }
    }

    /// The reads the rows added since the last round need.
    fn next_reads(&mut self) -> Vec<ReadBatch> {
        let mut needed: BTreeMap<(Scope, Edge), BTreeSet<String>> = BTreeMap::new();
        for index in self.frontier.drain(..) {
            let row = &self.rows[index];
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
                if edge == Edge::Physical && self.seen.contains(&(scope.clone(), id.to_owned())) {
                    continue;
                }
                if self.queried.insert((scope.clone(), edge, id.to_owned())) {
                    needed
                        .entry((scope.clone(), edge))
                        .or_default()
                        .insert(id.to_owned());
                }
            }
        }
        needed
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
            .collect()
    }

    fn accept(&mut self, read: &ReadBatch, candidates: Vec<AgentRequestRow>) -> Result<()> {
        for row in candidates {
            anyhow::ensure!(
                read.scope.owns(&row),
                "request dependency query returned a row outside its exact scope"
            );
            let doc_id = row
                .doc_id
                .as_ref()
                .context("request dependency row lacks physical identity")?;
            if self.seen.insert((read.scope.clone(), doc_id.clone())) {
                self.frontier.push(self.rows.len());
                self.rows.push(row);
            }
        }
        Ok(())
    }

    /// Reads the next round's dependencies with `extra` in the same
    /// requests and returns `extra`'s rows, in its order; `None` once
    /// nothing is left to read.
    pub(super) async fn round<'a>(
        &mut self,
        extra: Vec<RequestRows>,
        execute: &mut ExecuteViews<'a>,
    ) -> Result<Option<Vec<Vec<AgentRequestRow>>>> {
        let reads = self.next_reads();
        if reads.is_empty() && extra.is_empty() {
            return Ok(None);
        }
        let extra_len = extra.len();
        let views = extra
            .into_iter()
            .chain(reads.iter().map(ReadBatch::view))
            .collect::<Vec<_>>();
        /* a round's documents go out a few at a time rather than one
        after another; their results are folded in view order, so the rows
        come out as the one-at-a-time walk produced them */
        let size = views.len().div_ceil(views.len().div_ceil(ROOT_BATCH_SIZE));
        let documents = views.chunks(size).collect::<Vec<_>>();
        let mut results = Vec::with_capacity(views.len());
        for wave in documents.chunks(CLOSURE_BATCHES_AT_ONCE) {
            let answers = futures::future::try_join_all(
                wave.iter().map(|document| execute(document.to_vec())),
            )
            .await?;
            for (document, answer) in wave.iter().zip(answers) {
                anyhow::ensure!(
                    answer.len() == document.len(),
                    "request dependency batch omitted results"
                );
                results.extend(answer);
            }
        }
        let mut results = results.into_iter();
        let extra = results.by_ref().take(extra_len).collect::<Vec<_>>();
        for (read, candidates) in reads.iter().zip(results) {
            self.accept(read, candidates)?;
        }
        Ok(Some(extra))
    }

    pub(super) fn finish(mut self) -> Vec<AgentRequestRow> {
        crate::client_projection::store::sort_request_rows(&mut self.rows);
        self.rows
    }
}

#[cfg(test)]
mod tests;
