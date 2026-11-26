use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use events::{DocumentChange, DocumentChangeBatch};
use tokio::sync::oneshot;

use crate::request::hooks::effect::{Access, DocRef, Effect, Manifest, Row, Write};
use crate::request::hooks::hook::{Hook, Outcome, Query};
use crate::request::hooks::stage::{ApplyFailure, HookError, Host};
use crate::request::interrupt::InterruptChangeFeed;
use crate::session::output::replay_request::{ReplayRequestFacts, ReplayRequestReader};
use crate::workspace::{
    IsolatedWorkspaceDoc, MemoryWorkspaceDocuments, WorkspaceBindingDoc, WorkspaceDocuments,
    WorkspacePlacementDoc, WorkspaceReceiptDoc,
};

pub(crate) struct FailFreezePlacementOnce {
    pub(crate) inner: MemoryWorkspaceDocuments,
    pub(crate) fail_placement: bool,
    pub(crate) workspace_writes: usize,
    pub(crate) placement_writes: usize,
}

impl WorkspaceDocuments for FailFreezePlacementOnce {
    fn load_isolated_workspace(&self, id: &str) -> anyhow::Result<Option<IsolatedWorkspaceDoc>> {
        self.inner.load_isolated_workspace(id)
    }

    fn load_placement(&self, id: &str) -> anyhow::Result<Option<WorkspacePlacementDoc>> {
        self.inner.load_placement(id)
    }

    fn write_isolated_workspace(&mut self, doc: IsolatedWorkspaceDoc) -> anyhow::Result<()> {
        self.workspace_writes += 1;
        self.inner.write_isolated_workspace(doc)
    }

    fn write_placement(&mut self, doc: WorkspacePlacementDoc) -> anyhow::Result<()> {
        self.placement_writes += 1;
        if std::mem::take(&mut self.fail_placement) {
            anyhow::bail!("injected placement persistence failure after workspace write");
        }
        self.inner.write_placement(doc)
    }

    fn load_bindings(&self, id: &str) -> anyhow::Result<Vec<WorkspaceBindingDoc>> {
        self.inner.load_bindings(id)
    }

    fn write_binding(&mut self, doc: WorkspaceBindingDoc) -> anyhow::Result<()> {
        self.inner.write_binding(doc)
    }

    fn load_receipts(&self, id: &str) -> anyhow::Result<Vec<WorkspaceReceiptDoc>> {
        self.inner.load_receipts(id)
    }

    fn write_receipt(&mut self, doc: WorkspaceReceiptDoc) -> anyhow::Result<()> {
        self.inner.write_receipt(doc)
    }
}

pub(crate) enum TestInterruptFeed {
    Flood {
        delivered: Arc<AtomicUsize>,
        limit: usize,
        doc_id: &'static str,
    },
    AdvancingFlood(&'static str),
    Once(Option<oneshot::Receiver<DocumentChangeBatch>>),
}

fn interrupt_batch(doc_id: &str) -> DocumentChangeBatch {
    DocumentChangeBatch {
        changes: vec![DocumentChange {
            collection_id: "AgentRequest".into(),
            doc_id: doc_id.into(),
            has_local_write: true,
        }],
        resync_required: false,
        updates: 1,
    }
}

impl InterruptChangeFeed for TestInterruptFeed {
    async fn recv(&mut self) -> Option<DocumentChangeBatch> {
        match self {
            Self::Flood {
                delivered,
                limit,
                doc_id,
            } => {
                if delivered.fetch_add(1, Ordering::SeqCst) >= *limit {
                    None
                } else {
                    Some(interrupt_batch(doc_id))
                }
            }
            Self::AdvancingFlood(doc_id) => {
                tokio::time::advance(Duration::from_millis(100)).await;
                Some(interrupt_batch(doc_id))
            }
            Self::Once(rx) => match rx.take() {
                Some(rx) => rx.await.ok(),
                None => std::future::pending().await,
            },
        }
    }
}

/// The filter field naming the plugin a declared read belongs to, so the
/// store double can fail exactly the reads the model marks as failed.
pub(crate) const PLUGIN_FIELD: &str = "plugin";

/// Lean `Hooks.State` as the store double holds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) core: Vec<String>,
    pub(crate) rows: Vec<Row>,
}

/// An in-memory lifecycle transaction. Row order follows Lean `Store`: a
/// create prepends, an update rewrites in place and a delete filters.
pub(crate) struct StoreDouble {
    state: Mutex<Snapshot>,
    denied: Vec<(Access, DocRef)>,
    failing_ops: Vec<String>,
    unreadable: Vec<String>,
    failed_operation: Mutex<Option<&'static str>>,
}

impl StoreDouble {
    pub(crate) fn new(
        core: Vec<String>,
        rows: Vec<Row>,
        denied: Vec<(Access, DocRef)>,
        failing_ops: Vec<String>,
        unreadable: Vec<String>,
    ) -> Self {
        Self {
            state: Mutex::new(Snapshot { core, rows }),
            denied,
            failing_ops,
            unreadable,
            failed_operation: Mutex::new(None),
        }
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().clone()
    }

    pub(crate) fn permits(&self, access: Access, doc: &DocRef) -> bool {
        !self
            .denied
            .iter()
            .any(|(denied_access, denied_doc)| *denied_access == access && denied_doc == doc)
    }

    pub(crate) fn failed_operation(&self) -> Option<&'static str> {
        *self.failed_operation.lock().unwrap()
    }

    /// Lean `Hooks.applyWrite`.
    fn write(rows: &mut Vec<Row>, write: &Write) -> bool {
        match write {
            Write::Create { doc, value, .. } => {
                if rows.iter().any(|row| &row.doc == doc) {
                    return false;
                }
                rows.insert(
                    0,
                    Row {
                        doc: doc.clone(),
                        revision: 0,
                        value: value.clone(),
                    },
                );
                true
            }
            Write::Update {
                doc,
                expected,
                value,
                ..
            } => {
                let mut held = false;
                for row in rows.iter_mut() {
                    if &row.doc == doc && row.revision == *expected {
                        row.revision = expected + 1;
                        row.value = value.clone();
                        held = true;
                    }
                }
                held
            }
            Write::Delete { doc, expected, .. } => {
                if !rows
                    .iter()
                    .any(|row| &row.doc == doc && row.revision == *expected)
                {
                    return false;
                }
                rows.retain(|row| &row.doc != doc);
                true
            }
        }
    }

    /// Lean `Hooks.applyEffects` with `World.perform`: all or none.
    fn applied(&self, plugin: &'static str, effects: &[Effect]) -> Result<Snapshot, HookError> {
        let mut next = self.snapshot();
        for effect in effects {
            match effect {
                Effect::Write(write) => {
                    if !Self::write(&mut next.rows, write) {
                        return Err(HookError::Conflict {
                            plugin,
                            message: write.conflict().map(str::to_owned),
                        });
                    }
                }
                Effect::Core(operation) => {
                    if self.failing_ops.contains(&operation.name) {
                        *self.failed_operation.lock().unwrap() = Some(plugin);
                        return Err(HookError::Operation(anyhow::anyhow!(
                            "core refused {}",
                            operation.name
                        )));
                    }
                    next.core.push(operation.name.clone());
                }
            }
        }
        Ok(next)
    }
}

#[async_trait::async_trait]
impl Host for StoreDouble {
    fn actor(&self) -> Option<String> {
        None
    }

    async fn select(
        &self,
        _manifest: &Manifest,
        _named: &[DocRef],
        query: &Query,
    ) -> Result<Vec<Row>, HookError> {
        let unreadable = query.filter.iter().any(|(field, value)| {
            field == PLUGIN_FIELD
                && value
                    .as_str()
                    .is_some_and(|plugin| self.unreadable.iter().any(|name| name == plugin))
        });
        if unreadable {
            return Err(HookError::Query(format!(
                "{} rows are unavailable",
                query.collection
            )));
        }
        Ok(self
            .snapshot()
            .rows
            .into_iter()
            .filter(|row| row.doc.collection == query.collection)
            .collect())
    }

    async fn allowed(&self, access: Access, doc: &DocRef) -> Result<bool, HookError> {
        Ok(self.permits(access, doc))
    }

    async fn apply(&self, manifest: &Manifest, effects: &[Effect]) -> Result<(), ApplyFailure> {
        let next = self
            .applied(manifest.name, effects)
            .map_err(|error| ApplyFailure { clean: true, error })?;
        *self.state.lock().unwrap() = next;
        Ok(())
    }
}

/// Lean `Conformance.HooksContracts.Decision` rebuilt over production values.
pub(crate) enum ScriptedDecision<V> {
    Refuse(String),
    Proceed(V, Vec<Effect>),
    ByShown {
        count: usize,
        matching: (V, Vec<Effect>),
        otherwise: (V, Vec<Effect>),
    },
}

pub(crate) struct ScriptedHook<V> {
    pub(crate) manifest: Manifest,
    pub(crate) reads: Vec<String>,
    pub(crate) decision: ScriptedDecision<V>,
}

impl<V> Hook<()> for ScriptedHook<V>
where
    V: Clone + Send + Sync,
{
    type Value = V;

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reads(&self, _event: &()) -> Vec<Query> {
        self.reads
            .iter()
            .map(|collection| {
                Query::select(collection.clone(), "value").eq(PLUGIN_FIELD, self.manifest.name)
            })
            .collect()
    }

    fn decide(&self, _event: &(), rows: &[Vec<Row>]) -> Outcome<V> {
        let (value, effects) = match &self.decision {
            ScriptedDecision::Refuse(reason) => return Outcome::Refuse(reason.clone()),
            ScriptedDecision::Proceed(value, effects) => (value, effects),
            ScriptedDecision::ByShown {
                count,
                matching,
                otherwise,
            } => {
                let shown: usize = rows.iter().map(Vec::len).sum();
                let chosen = if shown == *count { matching } else { otherwise };
                (&chosen.0, &chosen.1)
            }
        };
        Outcome::Proceed {
            value: value.clone(),
            effects: effects.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum RequestFault {
    Missing,
    RowsReadFails,
    CommitsReadFails,
}

pub(crate) struct FaultingRequests<'a> {
    pub(crate) access: &'a gents_store::config_client::ConfigAccess,
    pub(crate) request_doc_id: &'a str,
    pub(crate) fault: Option<RequestFault>,
}

pub(crate) struct CountingRequests<'a> {
    pub(crate) access: &'a gents_store::config_client::ConfigAccess,
    pub(crate) rows: AtomicUsize,
    pub(crate) commits: AtomicUsize,
}

#[async_trait::async_trait]
impl ReplayRequestReader for CountingRequests<'_> {
    async fn request_facts(
        &self,
        ids: &[String],
    ) -> anyhow::Result<BTreeMap<String, ReplayRequestFacts>> {
        self.commits.fetch_add(ids.len(), Ordering::Relaxed);
        self.rows.fetch_add(ids.len(), Ordering::Relaxed);
        self.access.request_facts(ids).await
    }
}

#[async_trait::async_trait]
impl ReplayRequestReader for FaultingRequests<'_> {
    async fn request_facts(
        &self,
        request_doc_ids: &[String],
    ) -> anyhow::Result<BTreeMap<String, ReplayRequestFacts>> {
        if request_doc_ids.iter().any(|id| id == self.request_doc_id) {
            match self.fault {
                Some(RequestFault::RowsReadFails) => anyhow::bail!("injected AgentRequest read"),
                Some(RequestFault::CommitsReadFails) => {
                    anyhow::bail!("injected composite commit read")
                }
                _ => {}
            }
        }
        let mut facts = self.access.request_facts(request_doc_ids).await?;
        if matches!(self.fault, Some(RequestFault::Missing)) {
            if let Some(request) = facts.get_mut(self.request_doc_id) {
                request.rows.clear();
            }
        }
        Ok(facts)
    }
}
