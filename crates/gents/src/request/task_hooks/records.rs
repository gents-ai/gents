use rapidhash::RapidHashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use serde::{Deserialize, Serialize};

use tokio_util::sync::CancellationToken;

use super::{run_finally, HookCommandResult, ManagedTaskHookExec, TaskHookCancellation};
use crate::documents::{TaskHook, TaskHookPhase};
use crate::store::graphql::escape_graphql_string;
use crate::tools::exec::ownership::{
    HostRecord, HostRecordStore, ProcessIdentity, ProcessRecorder,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskHookRecord {
    pub(crate) request_doc_id: String,
    pub(crate) request_id: String,
    pub(crate) node_did: String,
    pub(crate) agent_did: String,
    pub(crate) cwd: PathBuf,
    #[serde(default)]
    pub(crate) root_guard: Option<crate::tools::surface::RootExecutionGuard>,
    pub(crate) hooks: Vec<TaskHook>,
    #[serde(default)]
    pub(crate) attempts: Vec<RecordedHookAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecordedHookAttempt {
    pub(crate) hook_id: String,
    #[serde(default)]
    pub(crate) process: Option<ProcessIdentity>,
    #[serde(default)]
    pub(crate) result: Option<HookCommandResult>,
}

impl TaskHookRecord {
    fn remaining_cleanup(&self) -> Vec<TaskHook> {
        self.hooks
            .iter()
            .filter(|hook| {
                hook.phase == TaskHookPhase::Finally
                    && !self
                        .attempts
                        .iter()
                        .any(|attempt| attempt.hook_id == hook.hook_id)
            })
            .cloned()
            .collect()
    }
}

impl HostRecord for TaskHookRecord {
    fn key(&self) -> &str {
        &self.request_doc_id
    }
}

#[derive(Clone)]
pub(crate) struct TaskHookRecordStore {
    storage: HostRecordStore<TaskHookRecord>,
    live: Arc<Mutex<RapidHashSet<String>>>,
    recoveries: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    shutdown: CancellationToken,
}

impl Default for TaskHookRecordStore {
    fn default() -> Self {
        Self::with_storage(HostRecordStore::default())
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TaskHookRecordStore {
    fn with_storage(storage: HostRecordStore<TaskHookRecord>) -> Self {
        Self {
            storage,
            live: Arc::default(),
            recoveries: Arc::default(),
            shutdown: CancellationToken::new(),
        }
    }

    pub(crate) fn durable(dir: PathBuf) -> Self {
        Self::with_storage(HostRecordStore::Durable(dir))
    }

    pub(crate) async fn wait_for_recoveries(&self) {
        loop {
            let pending = std::mem::take(&mut *locked(&self.recoveries));
            if pending.is_empty() {
                return;
            }
            for recovery in pending {
                if let Err(error) = recovery.await {
                    tracing::error!(%error, "task hook recovery task failed");
                }
            }
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown.cancel();
        self.wait_for_recoveries().await;
    }

    fn write(&self, record: &TaskHookRecord) -> std::io::Result<()> {
        self.storage.write(record)
    }

    fn remove(&self, request_doc_id: &str) {
        self.storage.remove(request_doc_id);
    }

    pub(crate) fn list(&self) -> Vec<TaskHookRecord> {
        self.storage.list()
    }

    fn claim(&self, record: TaskHookRecord) -> Option<TaskHookRecordHandle> {
        if !locked(&self.live).insert(record.request_doc_id.clone()) {
            return None;
        }
        Some(TaskHookRecordHandle(Arc::new(HandleInner {
            store: self.clone(),
            record: Mutex::new(record),
        })))
    }

    pub(crate) fn begin(&self, record: TaskHookRecord) -> std::io::Result<TaskHookRecordHandle> {
        let key = record.request_doc_id.clone();
        self.claim(record).ok_or_else(|| {
            std::io::Error::other(format!("task hook record {key} is already owned"))
        })
    }
}

struct HandleInner {
    store: TaskHookRecordStore,
    record: Mutex<TaskHookRecord>,
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        let key = locked(&self.record).request_doc_id.clone();
        locked(&self.store.live).remove(&key);
    }
}

#[derive(Clone)]
pub(crate) struct TaskHookRecordHandle(Arc<HandleInner>);

fn current_attempt<'a>(
    record: &'a mut TaskHookRecord,
    hook_id: &str,
) -> Option<&'a mut RecordedHookAttempt> {
    record
        .attempts
        .iter_mut()
        .rev()
        .find(|attempt| attempt.hook_id == hook_id)
}

impl TaskHookRecordHandle {
    fn update_now(&self, change: impl FnOnce(&mut TaskHookRecord)) -> std::io::Result<()> {
        let mut record = locked(&self.0.record);
        change(&mut record);
        self.0.store.write(&record)
    }

    /// Durable writes sync the file and directory, so they run on the blocking pool.
    async fn update(
        &self,
        change: impl FnOnce(&mut TaskHookRecord) + Send + 'static,
    ) -> std::io::Result<()> {
        let handle = self.clone();
        tokio::task::spawn_blocking(move || handle.update_now(change))
            .await
            .map_err(std::io::Error::other)?
    }

    pub(crate) async fn work_started(&self) -> std::io::Result<()> {
        self.update(|_| {}).await
    }

    pub(crate) async fn attempt_started(&self, hook_id: &str) -> std::io::Result<()> {
        let hook_id = hook_id.to_owned();
        self.update(move |record| {
            record.attempts.push(RecordedHookAttempt {
                hook_id,
                process: None,
                result: None,
            })
        })
        .await
    }

    pub(crate) fn process_recorder(&self, hook_id: &str) -> ProcessRecorder {
        let handle = self.clone();
        let hook_id = hook_id.to_owned();
        ProcessRecorder::new(move |identity| {
            if let Err(error) = handle.update_now(|record| {
                if let Some(attempt) = current_attempt(record, &hook_id) {
                    attempt.process = Some(identity);
                }
            }) {
                tracing::warn!(hook_id, %error, "failed to record task hook process ownership");
            }
        })
    }

    pub(crate) async fn attempt_finished(&self, hook_id: &str, result: &HookCommandResult) {
        let key = hook_id.to_owned();
        let result = result.clone();
        if let Err(error) = self
            .update(move |record| {
                if let Some(attempt) = current_attempt(record, &key) {
                    attempt.result = Some(result);
                }
            })
            .await
        {
            tracing::warn!(hook_id, %error, "failed to record task hook result");
        }
    }

    pub(crate) async fn attempt_refused(&self, hook_id: &str, result: HookCommandResult) {
        let key = hook_id.to_owned();
        if let Err(error) = self
            .update(move |record| {
                record.attempts.push(RecordedHookAttempt {
                    hook_id: key,
                    process: None,
                    result: Some(result),
                })
            })
            .await
        {
            tracing::warn!(hook_id, %error, "failed to record refused task hook");
        }
    }

    pub(crate) async fn release(&self) -> bool {
        let handle = self.clone();
        tokio::task::spawn_blocking(move || {
            let record = locked(&handle.0.record);
            let done = record.remaining_cleanup().is_empty();
            if done {
                handle.0.store.remove(&record.request_doc_id);
            }
            done
        })
        .await
        .unwrap_or(false)
    }
}

async fn request_resolved(access: &crate::ConfigAccess, record: &TaskHookRecord) -> Result<bool> {
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<gents_protocol::request_lifecycle::RequestLifecycleState>,
    }
    let doc_id = escape_graphql_string(&record.request_doc_id);
    let node_did = escape_graphql_string(&record.node_did);
    let agent_did = escape_graphql_string(&record.agent_did);
    let response = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{node_did}" }}, agent_did: {{ _eq: "{agent_did}" }} }}, limit: 2) {{ lifecycle_state }} }}"#,
    )).await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("task hook recovery query omitted request rows")?;
    let [row] = rows.as_slice() else {
        return Ok(false);
    };
    let row: Row = serde_json::from_value(row.clone())?;
    Ok(row.lifecycle_state.is_some_and(|state| state.is_terminal()))
}

async fn recover_record(
    node: Arc<EmbeddedNode>,
    record: TaskHookRecord,
    handle: TaskHookRecordHandle,
    agent_identity: Option<Arc<dyn crate::store::identity::NodeIdentity>>,
    shutdown: CancellationToken,
) {
    for attempt in record
        .attempts
        .iter()
        .filter(|attempt| attempt.result.is_none())
    {
        if let Some(process) = &attempt.process {
            let (before, after) = process.stop().await;
            tracing::info!(
                request_id = %record.request_id,
                hook_id = %attempt.hook_id,
                outcome = crate::tools::exec::ProcessStopOutcome::from_observations(before, after).as_str(),
                "stopped task hook command left by an interrupted execution"
            );
        }
    }
    let remaining = record.remaining_cleanup();
    if remaining.is_empty() {
        handle.release().await;
        return;
    }
    let admitted = match &record.root_guard {
        Some(guard) => match agent_identity.as_deref() {
            Some(identity) => guard.validate(&node, identity).await,
            None => {
                tracing::warn!(agent_did = %guard.agent_did, "task hook cleanup awaits the selected Agent identity");
                return;
            }
        },
        None => Ok(()),
    };
    if let Err(error) = admitted {
        tracing::error!(
            request_id = %record.request_id,
            cwd = %record.cwd.display(),
            error = %format!("{error:#}"),
            "refusing recovered task cleanup outside an admitted root"
        );
        for hook in &remaining {
            handle
                .attempt_refused(&hook.hook_id, HookCommandResult::LaunchFailed)
                .await;
        }
        handle.release().await;
        return;
    }
    let exec = ManagedTaskHookExec::new(record.cwd.clone(), TaskHookCancellation::under(&shutdown))
        .with_record(Some(handle.clone()));
    for attempt in run_finally(&remaining, &exec)
        .await
        .iter()
        .filter(|attempt| !attempt.result.succeeded())
    {
        tracing::warn!(
            request_id = %record.request_id,
            hook_id = %attempt.hook_id,
            detail = %attempt.detail,
            "recovered task cleanup hook did not succeed"
        );
    }
    handle.release().await;
}

/// Recovery stops a process only when its recorded identity proves this runtime owns it.
pub(crate) async fn recover_task_hook_records(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[Arc<dyn crate::store::identity::NodeIdentity>],
    store: &TaskHookRecordStore,
) -> Result<usize> {
    let mut started = 0;
    if store.shutdown.is_cancelled() {
        return Ok(started);
    }
    let records = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || store.list())
            .await
            .context("enumerating retained task hook records")?
    };
    if store.shutdown.is_cancelled() {
        return Ok(started);
    }
    for record in records {
        if record.node_did != node_did {
            continue;
        }
        let Some(agent_identity) = identities
            .iter()
            .find(|identity| identity.did() == record.agent_did)
            .cloned()
        else {
            tracing::warn!(agent_did = %record.agent_did, "task hook recovery awaits the selected Agent identity");
            continue;
        };
        if record
            .root_guard
            .as_ref()
            .is_some_and(|guard| guard.agent_did != record.agent_did)
        {
            tracing::warn!(agent_did = %record.agent_did, "task hook recovery root belongs to another Agent");
            continue;
        }
        let actor = match identity::Did::new(&record.agent_did) {
            Ok(actor) => actor,
            Err(error) => {
                tracing::warn!(agent_did = %record.agent_did, %error, "task hook recovery has an invalid Agent DID");
                continue;
            }
        };
        let access = match crate::ConfigAccess::local_managed_agent(
            node.clone(),
            actor,
            agent_identity.as_ref(),
        ) {
            Ok(access) => access,
            Err(error) => {
                tracing::warn!(agent_did = %record.agent_did, %error, "task hook recovery could not authenticate its Agent");
                continue;
            }
        };
        let Some(handle) = store.claim(record.clone()) else {
            continue;
        };
        match request_resolved(&access, &record).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                tracing::warn!(request_id = %record.request_id, %error, "task hook recovery could not read its request; retrying next pass");
                continue;
            }
        }
        started += 1;
        let recovery = tokio::spawn(recover_record(
            node.clone(),
            record,
            handle,
            Some(agent_identity),
            store.shutdown.clone(),
        ));
        let mut recoveries = locked(&store.recoveries);
        recoveries.retain(|recovery| !recovery.is_finished());
        recoveries.push(recovery);
    }
    Ok(started)
}
