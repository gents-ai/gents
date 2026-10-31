use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::inference::llm::message::Message;
use crate::inference::llm::tool::ToolDyn;
use crate::inference::llm::{HookAction, ToolCallHookAction};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::session;
use crate::tools::background::status::LiveToolOutputRegistry;
use crate::tools::call::lifecycle::{
    AwaitMode, CancelCause, InterruptDisposition, ToolCallLifecycle,
};
use crate::tools::mcp::meta::selected_remote_identity;
use crate::truncation::TruncationLimits;

pub(crate) mod persistence;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailurePolicy {
    FailOpen,
    #[default]
    FailClosed,
}

#[derive(Debug)]
pub struct HookStats {
    pub persistence_failures: u64,
    pub persistence_successes: u64,
}

struct HookCounters {
    failures: AtomicU64,
    successes: AtomicU64,
}

#[derive(Clone, Default)]
pub struct BackgroundToolRegistry {
    inner: Arc<BackgroundToolRegistryInner>,
}

#[derive(Default)]
struct BackgroundToolRegistryInner {
    tools: RapidHashMap<String, Arc<dyn ToolDyn>>,
    allowlist: Vec<String>,
    timeouts: crate::tools::surface::BackgroundTimeouts,
}

impl BackgroundToolRegistry {
    pub fn from_tools(tools: Vec<Box<dyn ToolDyn>>, allowlist: &[String]) -> Self {
        let allowed = allowlist
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect::<RapidHashSet<_>>();
        let mut registry_tools = RapidHashMap::new();
        for tool in tools {
            let name = tool.name();
            if allowed.contains(&name) {
                registry_tools.insert(name, Arc::from(tool));
            }
        }
        let mut allowlist = allowed.into_iter().collect::<Vec<_>>();
        allowlist.sort();
        Self {
            inner: Arc::new(BackgroundToolRegistryInner {
                tools: registry_tools,
                allowlist,
                timeouts: Default::default(),
            }),
        }
    }

    pub(crate) fn from_config(
        tools: Vec<Box<dyn ToolDyn>>,
        config: &crate::tools::surface::BackgroundToolConfig,
    ) -> Self {
        let registry = Self::from_tools(tools, &config.allowlist);
        let inner = Arc::into_inner(registry.inner)
            .expect("a freshly built background registry has one owner");
        Self {
            inner: Arc::new(BackgroundToolRegistryInner {
                timeouts: config.timeouts.clone(),
                ..inner
            }),
        }
    }

    pub(crate) fn timeouts(&self) -> &crate::tools::surface::BackgroundTimeouts {
        &self.inner.timeouts
    }

    pub(crate) fn get(&self, tool_name: &str) -> Option<Arc<dyn ToolDyn>> {
        self.inner.tools.get(tool_name).cloned()
    }

    pub(crate) fn allowlist(&self) -> Vec<String> {
        self.inner.allowlist.clone()
    }
}

#[derive(Clone)]
enum RetainedStopReceipt {
    Cancel(CancelCause, String),
    Timeout,
    Failure(crate::tools::call::lifecycle::ToolOutcome),
    FailurePending,
}

impl RetainedStopReceipt {
    fn from_outcome(outcome: &crate::tools::call::lifecycle::ToolOutcome) -> Self {
        match outcome {
            crate::tools::call::lifecycle::ToolOutcome::Cancelled => {
                Self::Cancel(CancelCause::Interrupted, "request interrupted".to_owned())
            }
            crate::tools::call::lifecycle::ToolOutcome::TimedOut { .. } => Self::Timeout,
            crate::tools::call::lifecycle::ToolOutcome::Failed { .. } => {
                Self::Failure(outcome.clone())
            }
            crate::tools::call::lifecycle::ToolOutcome::Completed(_) => {
                unreachable!("completion does not abandon a native future")
            }
        }
    }
}

#[derive(Clone)]
struct BackgroundExecution {
    tool_call_id: String,
    cancellation_token: CancellationToken,
    completion_tx: watch::Sender<bool>,
    worker_release_tx: watch::Sender<bool>,
    tool_call_doc_id: String,
    process_evidence: Arc<crate::tools::exec::ownership::LiveProcessEvidenceOwner>,
    process_identity: Arc<std::sync::Mutex<Option<crate::tools::exec::ownership::ProcessIdentity>>>,
    cancellation_reason: Arc<std::sync::Mutex<Option<String>>>,
    recovery_lifecycle: Arc<std::sync::Mutex<Option<ToolCallLifecycle>>>,
    retained: Arc<std::sync::atomic::AtomicBool>,
    handoff_pending: Arc<std::sync::atomic::AtomicBool>,
    requested_stop: Arc<std::sync::Mutex<Option<RetainedStopReceipt>>>,
    confirmed_stop: Arc<std::sync::Mutex<Option<RetainedStopReceipt>>>,
    _service_work: Arc<crate::runtime_service::RuntimeServiceWork>,
}

#[derive(Clone, Default)]
struct BackgroundLiveOutputState {
    registry: LiveToolOutputRegistry,
}

impl BackgroundLiveOutputState {
    async fn writer_for(
        &self,
        tool_call_id: impl Into<String>,
    ) -> crate::tools::background::status::LiveToolOutputWriter {
        self.registry.writer_for(tool_call_id).await
    }

    async fn canonical_writer_for(
        &self,
        binding: crate::tools::call::lifecycle::delivery::ToolOutputBinding,
    ) -> crate::tools::background::status::LiveToolOutputWriter {
        let tool_call_doc_id = binding.tool_call_doc_id.clone();
        self.registry
            .canonical_writer_for(tool_call_doc_id, Arc::new(binding))
            .await
    }

    async fn remove(&self, tool_call_id: &str) {
        self.registry.remove(tool_call_id).await;
    }
}

#[derive(Clone, Default)]
pub struct BackgroundExecutionRegistry {
    inner: Arc<std::sync::Mutex<RapidHashMap<String, BackgroundExecution>>>,
    live_outputs: BackgroundLiveOutputState,
    process_records: crate::tools::exec::ownership::ProcessRecordStore,
    #[cfg(windows)]
    failed_jobs: Arc<
        std::sync::Mutex<RapidHashMap<String, (String, Arc<crate::tools::exec::FailedJobOwner>)>>,
    >,
    task_hook_records: crate::request::task_hooks::TaskHookRecordStore,
    service_admission: crate::runtime_service::RuntimeServiceAdmission,
    #[cfg(test)]
    stop_release_observer: Option<Arc<tokio::sync::Notify>>,
}

pub(crate) const LIVE_STOP_RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

impl BackgroundExecutionRegistry {
    #[cfg(test)]
    pub(crate) fn with_stop_release_observer(mut self, observer: Arc<tokio::sync::Notify>) -> Self {
        self.stop_release_observer = Some(observer);
        self
    }

    pub(crate) fn service_admission(&self) -> &crate::runtime_service::RuntimeServiceAdmission {
        &self.service_admission
    }

    /// Keep durable host records of spawned background processes in `dir`,
    /// which must belong to this runtime's exclusively locked store; a
    /// restarted runtime otherwise cannot prove it owns a surviving process.
    pub fn with_process_records(mut self, dir: PathBuf) -> Self {
        self.process_records = crate::tools::exec::ownership::ProcessRecordStore::Durable(dir);
        self
    }

    pub fn with_task_hook_records(mut self, dir: PathBuf) -> Self {
        self.task_hook_records = crate::request::task_hooks::TaskHookRecordStore::durable(dir);
        self
    }

    pub(crate) fn task_hook_records(&self) -> &crate::request::task_hooks::TaskHookRecordStore {
        &self.task_hook_records
    }

    pub(crate) fn process_recorder(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> crate::tools::exec::ownership::ProcessRecorder {
        let store = self.process_records.clone();
        #[cfg(windows)]
        let failed_jobs = self.failed_jobs.clone();
        #[cfg(windows)]
        let release_failed_jobs = self.failed_jobs.clone();
        #[cfg(windows)]
        let release_failed_doc = tool_call_doc_id.to_owned();
        #[cfg(windows)]
        let failed_job_call = tool_call_id.to_owned();
        #[cfg(windows)]
        let failed_job_doc = tool_call_doc_id.to_owned();
        let tool_call_id = tool_call_id.to_owned();
        let tool_call_doc_id = tool_call_doc_id.to_owned();
        let execution = self
            .lock_executions()
            .get(&tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .cloned();
        let evidence = execution
            .as_ref()
            .map(|execution| execution.process_evidence.clone());
        let identity_owner = execution
            .as_ref()
            .map(|execution| execution.process_identity.clone());
        let failure_owner = execution.map(|execution| execution.requested_stop);
        let recorder = crate::tools::exec::ownership::ProcessRecorder::new(move |identity| {
            if let Some(owner) = &identity_owner {
                *owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(identity.clone());
            }
            let record = crate::tools::exec::ownership::ProcessRecord {
                tool_call_id: tool_call_id.clone(),
                tool_call_doc_id: tool_call_doc_id.clone(),
                identity,
            };
            if let Err(error) = store.write(&record) {
                tracing::warn!(
                        tool_call_id = %record.tool_call_id,
                        %error,
                    "failed to record background process ownership; a runtime restart will settle it as lost"
                );
            }
        });
        let recorder = recorder.with_failure_decision(move || {
            if let Some(owner) = &failure_owner {
                let mut first = owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if first.is_none() {
                    *first = Some(RetainedStopReceipt::FailurePending);
                }
            }
        });
        #[cfg(windows)]
        let recorder = recorder.with_failed_job_owner(
            move |owner| {
                failed_jobs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(failed_job_doc.clone(), (failed_job_call.clone(), owner));
            },
            move || {
                release_failed_jobs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&release_failed_doc);
            },
        );
        match evidence {
            Some(evidence) => recorder.with_evidence(evidence),
            None => recorder,
        }
    }

    pub(crate) fn process_record(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> Option<crate::tools::exec::ownership::ProcessRecord> {
        [tool_call_doc_id, tool_call_id]
            .into_iter()
            .find_map(|key| {
                self.process_records.read(key).filter(|record| {
                    record.tool_call_id == tool_call_id
                        && record.tool_call_doc_id == tool_call_doc_id
                })
            })
            .or_else(|| {
                let execution = self
                    .lock_executions()
                    .get(tool_call_doc_id)
                    .filter(|execution| execution.tool_call_id == tool_call_id)
                    .cloned()?;
                let identity = execution
                    .process_identity
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()?;
                Some(crate::tools::exec::ownership::ProcessRecord {
                    tool_call_id: tool_call_id.to_owned(),
                    tool_call_doc_id: tool_call_doc_id.to_owned(),
                    identity,
                })
            })
    }

    pub(crate) fn live_process_evidence(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> crate::tools::exec::ownership::LiveProcessEvidence {
        self.lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .map(|execution| execution.process_evidence.get())
            .unwrap_or(crate::tools::exec::ownership::LiveProcessEvidence::SpawnedUnverified)
    }

    fn live_process_evidence_owner(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> Option<Arc<crate::tools::exec::ownership::LiveProcessEvidenceOwner>> {
        self.lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .map(|execution| execution.process_evidence.clone())
    }

    async fn process_exit_confirmed(&self, tool_call_id: &str, tool_call_doc_id: &str) -> bool {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            matches!(
                record
                    .identity
                    .await_signalled_group_exit(crate::tools::exec::ownership::KILL_GRACE)
                    .await,
                crate::tools::exec::ownership::ProcessObservation::Exited
            )
        } else {
            self.live_process_evidence(tool_call_id, tool_call_doc_id)
                != crate::tools::exec::ownership::LiveProcessEvidence::SpawnedUnverified
        }
    }

    pub(crate) fn set_cancellation_reason(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
        reason: &str,
        deadline_expired: bool,
    ) {
        self.set_stop_receipt(
            tool_call_id,
            tool_call_doc_id,
            if deadline_expired {
                RetainedStopReceipt::Timeout
            } else {
                RetainedStopReceipt::Cancel(CancelCause::UserCancelled, reason.to_owned())
            },
        );
        if let Some(execution) = self
            .lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
        {
            *execution
                .cancellation_reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.to_owned());
        }
    }

    fn set_stop_receipt(&self, call: &str, doc: &str, receipt: RetainedStopReceipt) {
        if let Some(execution) = self
            .lock_executions()
            .get(doc)
            .filter(|entry| entry.tool_call_id == call)
        {
            let mut first = execution
                .requested_stop
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if first.is_none()
                || (matches!(first.as_ref(), Some(RetainedStopReceipt::FailurePending))
                    && matches!(&receipt, RetainedStopReceipt::Failure(_)))
            {
                *first = Some(receipt.clone());
            }
            drop(first);
            if matches!(&receipt, RetainedStopReceipt::Failure(_)) {
                let mut confirmed = execution
                    .confirmed_stop
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if matches!(
                    confirmed.as_ref(),
                    Some(RetainedStopReceipt::FailurePending)
                ) {
                    *confirmed = Some(receipt);
                }
            }
        }
    }

    fn remember_confirmed_stop(&self, execution: &BackgroundExecution) {
        let receipt = execution
            .requested_stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| {
                RetainedStopReceipt::Cancel(
                    CancelCause::Interrupted,
                    "request interrupted".to_owned(),
                )
            });
        let mut slot = execution
            .confirmed_stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(receipt);
        }
    }

    fn finalize_pending_failure(
        &self,
        call: &str,
        doc: &str,
    ) -> Option<crate::tools::call::lifecycle::ToolOutcome> {
        if !self.has_pending_failure(call, doc) {
            return None;
        }
        let outcome = crate::tools::call::lifecycle::ToolOutcome::tool_returned_error(
            "native process failed during cleanup",
        );
        self.set_stop_receipt(call, doc, RetainedStopReceipt::Failure(outcome.clone()));
        Some(outcome)
    }

    pub(crate) fn has_pending_failure(&self, call: &str, doc: &str) -> bool {
        self.lock_executions()
            .get(doc)
            .filter(|entry| entry.tool_call_id == call)
            .is_some_and(|entry| {
                matches!(
                    entry
                        .requested_stop
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref(),
                    Some(RetainedStopReceipt::FailurePending)
                )
            })
    }

    pub(crate) fn retire_confirmed_stop(
        &self,
        tool_call_id: &str,
        doc: &str,
        terminal_committed: bool,
    ) {
        let retire = self.lock_executions().get(doc).is_some_and(|execution| {
            execution.tool_call_id == tool_call_id
                && execution.retained.load(std::sync::atomic::Ordering::SeqCst)
                && execution
                    .confirmed_stop
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some()
        });
        if retire && crate::tools::exec::ownership::retire_stop_receipt(terminal_committed) {
            self.remove_now(doc);
        }
    }

    pub(crate) fn cancellation_reason(&self, tool_call_id: &str, tool_call_doc_id: &str) -> String {
        self.lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .and_then(|execution| {
                execution
                    .cancellation_reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .unwrap_or_else(|| "explicit_cancel".to_owned())
    }

    pub(crate) fn process_record_list(&self) -> Vec<crate::tools::exec::ownership::ProcessRecord> {
        self.process_records.list()
    }

    pub(crate) fn try_process_record_list(
        &self,
    ) -> anyhow::Result<Vec<crate::tools::exec::ownership::ProcessRecord>> {
        self.process_records.try_list()
    }

    pub(crate) async fn release_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            let observed = record
                .identity
                .await_leader_exit(crate::tools::exec::ownership::KILL_GRACE)
                .await;
            if observed.retain_process_record() {
                tracing::warn!(
                    tool_call_id,
                    pid = record.identity.pid,
                    observation = ?observed,
                    "background process group is not proven exited; keeping its record"
                );
                return;
            }
            self.remove_process_record(tool_call_id, tool_call_doc_id);
        }
    }

    pub(crate) fn forget_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            let observed = record.identity.observe();
            if observed.retain_process_record() {
                tracing::warn!(
                    tool_call_id,
                    pid = record.identity.pid,
                    observation = ?observed,
                    "background process group is not proven exited; keeping its record"
                );
                return;
            }
            self.remove_process_record(tool_call_id, tool_call_doc_id);
        }
    }

    fn remove_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        for key in [tool_call_doc_id, tool_call_id] {
            if self.process_records.read(key).is_some_and(|record| {
                record.tool_call_id == tool_call_id && record.tool_call_doc_id == tool_call_doc_id
            }) {
                self.process_records.remove(key);
            }
        }
    }

    pub(crate) async fn stop_execution(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> crate::tools::exec::ProcessStopOutcome {
        self.stop_execution_inner(tool_call_id, tool_call_doc_id, false)
            .await
    }

    pub(crate) async fn stop_native_process(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> crate::tools::exec::ProcessStopOutcome {
        self.stop_execution_inner(tool_call_id, tool_call_doc_id, true)
            .await
    }

    #[cfg(windows)]
    async fn stop_failed_job(&self, tool_call_id: &str, tool_call_doc_id: &str) -> Option<bool> {
        let owner = self
            .failed_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(tool_call_doc_id)
            .filter(|(id, _)| id == tool_call_id)
            .map(|(_, owner)| owner.clone())?;
        let confirmed = owner.stop_and_confirm().await;
        if confirmed {
            let mut jobs = self
                .failed_jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if jobs
                .get(tool_call_doc_id)
                .is_some_and(|(_, current)| Arc::ptr_eq(current, &owner))
            {
                jobs.remove(tool_call_doc_id);
            }
        }
        Some(confirmed)
    }

    async fn stop_execution_inner(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
        require_record: bool,
    ) -> crate::tools::exec::ProcessStopOutcome {
        use crate::tools::exec::ownership::ProcessObservation;
        use crate::tools::exec::ProcessStopOutcome;
        let record = self.process_record(tool_call_id, tool_call_doc_id);
        let execution = self
            .lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .cloned();
        if let Some(execution) = execution {
            let completion_rx = execution.worker_release_tx.subscribe();
            execution.cancellation_token.cancel();
            let released = tokio::time::timeout(
                LIVE_STOP_RELEASE_WAIT,
                Self::await_completion(completion_rx),
            )
            .await
            .is_ok();
            #[cfg(test)]
            if let Some(observer) = &self.stop_release_observer {
                observer.notify_one();
            }
            #[cfg(windows)]
            if let Some(confirmed) = self.stop_failed_job(tool_call_id, tool_call_doc_id).await {
                if confirmed {
                    execution.process_evidence.exited();
                }
                return if released && confirmed {
                    if execution.retained.load(std::sync::atomic::Ordering::SeqCst) {
                        self.remember_confirmed_stop(&execution);
                    }
                    ProcessStopOutcome::Stopped
                } else {
                    ProcessStopOutcome::StillRunning
                };
            }
            if released && execution.retained.load(std::sync::atomic::Ordering::SeqCst) {
                if execution
                    .confirmed_stop
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some()
                {
                    return ProcessStopOutcome::Stopped;
                }
                if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
                    let (before, after) = record.identity.stop().await;
                    let result = ProcessStopOutcome::from_observations(before, after);
                    if matches!(
                        result,
                        ProcessStopOutcome::Stopped | ProcessStopOutcome::AlreadyExited
                    ) {
                        execution.process_evidence.exited();
                        self.remember_confirmed_stop(&execution);
                    }
                    return result;
                }
            }
            let evidence = if !released {
                crate::tools::exec::ownership::LiveProcessEvidence::SpawnedUnverified
            } else if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
                match record
                    .identity
                    .await_signalled_group_exit(crate::tools::exec::ownership::KILL_GRACE)
                    .await
                {
                    ProcessObservation::Exited => {
                        crate::tools::exec::ownership::LiveProcessEvidence::Exited
                    }
                    _ => crate::tools::exec::ownership::LiveProcessEvidence::SpawnedUnverified,
                }
            } else if require_record {
                execution.process_evidence.get()
            } else {
                crate::tools::exec::ownership::LiveProcessEvidence::Exited
            };
            let outcome = crate::tools::exec::ownership::settle_live_cancel(released, evidence);
            if outcome == ProcessStopOutcome::Stopped
                && execution.retained.load(std::sync::atomic::Ordering::SeqCst)
            {
                self.remember_confirmed_stop(&execution);
            }
            return outcome;
        }
        #[cfg(windows)]
        if let Some(confirmed) = self.stop_failed_job(tool_call_id, tool_call_doc_id).await {
            return if confirmed {
                ProcessStopOutcome::Stopped
            } else {
                ProcessStopOutcome::StillRunning
            };
        }
        let Some(record) = record else {
            return ProcessStopOutcome::NotOwned;
        };
        let (before, after) = record.identity.stop().await;
        ProcessStopOutcome::from_observations(before, after)
    }

    pub async fn read_process_output_snapshot(
        &self,
        node: &EmbeddedNode,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        tool_call_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let caller = crate::tools::background::status::ProcessControlScope {
            request_id: String::new(),
            session_id: session_id.to_owned(),
            node_did: node_did.to_owned(),
            requester_did: requester_did.map(str::to_owned),
        };
        let result = crate::tools::background::status::read_tool_output_slice(
            node,
            &caller,
            &self.live_outputs.registry,
            tool_call_id,
            0,
            usize::MAX,
        )
        .await?;
        Ok(match result {
            crate::tools::background::status::ReadToolOutputOutcome::Found(page) => {
                Some(serde_json::to_value(page)?)
            }
            _ => None,
        })
    }

    pub(crate) fn reserve(
        &self,
        tool_call_id: String,
        tool_call_doc_id: String,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<BackgroundExecutionReservation> {
        let service_work = Arc::new(self.service_admission.admit()?);
        let (completion_tx, _) = watch::channel(false);
        let (worker_release_tx, _) = watch::channel(false);
        let mut executions = self.lock_executions();
        anyhow::ensure!(
            !executions.contains_key(&tool_call_doc_id),
            "background invocation already has an execution owner"
        );
        executions.insert(
            tool_call_doc_id.clone(),
            BackgroundExecution {
                tool_call_id,
                cancellation_token,
                completion_tx,
                worker_release_tx,
                tool_call_doc_id: tool_call_doc_id.clone(),
                process_evidence: Default::default(),
                process_identity: Default::default(),
                cancellation_reason: Default::default(),
                recovery_lifecycle: Default::default(),
                retained: Default::default(),
                handoff_pending: Default::default(),
                requested_stop: Default::default(),
                confirmed_stop: Default::default(),
                _service_work: service_work.clone(),
            },
        );
        Ok(BackgroundExecutionReservation {
            registry: self.clone(),
            tool_call_doc_id,
            armed: true,
            _service_work: service_work,
        })
    }

    #[cfg(test)]
    pub(crate) async fn remove(&self, tool_call_id: &str) {
        let docs = self
            .lock_executions()
            .iter()
            .filter(|(_, execution)| execution.tool_call_id == tool_call_id)
            .map(|(doc, _)| doc.clone())
            .collect::<Vec<_>>();
        for doc in docs {
            self.remove_now(&doc);
        }
    }

    pub(crate) async fn contains(&self, tool_call_id: &str, tool_call_doc_id: &str) -> bool {
        self.lock_executions()
            .get(tool_call_doc_id)
            .is_some_and(|execution| execution.tool_call_id == tool_call_id)
    }

    pub(crate) fn active_tool_call_doc_ids(&self) -> Vec<String> {
        self.lock_executions()
            .values()
            .map(|execution| execution.tool_call_doc_id.clone())
            .collect()
    }

    fn retain_confirmed_worker(
        &self,
        lifecycle: ToolCallLifecycle,
        reservation: BackgroundExecutionReservation,
        receipt: RetainedStopReceipt,
    ) {
        let call = lifecycle.tool_call_id().to_owned();
        let doc = lifecycle
            .doc_id()
            .expect("owned physical invocation")
            .to_owned();
        self.set_stop_receipt(&call, &doc, receipt);
        reservation.retain_stopped_worker(lifecycle, false);
        let entry = self.lock_executions().get(&doc).cloned();
        if let Some(entry) = entry {
            entry.process_evidence.exited();
            self.remember_confirmed_stop(&entry);
        }
    }

    async fn retain_terminal_settlement(
        &self,
        mut lifecycle: ToolCallLifecycle,
        reservation: BackgroundExecutionReservation,
        receipt: RetainedStopReceipt,
        confirmed: bool,
    ) {
        let call = lifecycle.tool_call_id().to_owned();
        let doc = lifecycle
            .doc_id()
            .expect("owned physical invocation")
            .to_owned();
        self.set_stop_receipt(&call, &doc, receipt);
        reservation.retain_stopped_worker(lifecycle.clone(), false);
        if confirmed {
            let entry = self.lock_executions().get(&doc).cloned();
            if let Some(entry) = entry {
                entry.process_evidence.exited();
                self.remember_confirmed_stop(&entry);
            }
            if let Err(error) = self.settle_retained_stop(&mut lifecycle).await {
                tracing::warn!(tool_call_doc_id = %doc, %error, "confirmed process stop settlement remains pending");
            }
        }
    }

    pub(crate) async fn settle_retained_stop(
        &self,
        lifecycle: &mut ToolCallLifecycle,
    ) -> anyhow::Result<bool> {
        let Some(doc) = lifecycle.doc_id().map(str::to_owned) else {
            return Ok(false);
        };
        let entry = self
            .lock_executions()
            .get(&doc)
            .filter(|entry| entry.tool_call_id == lifecycle.tool_call_id())
            .cloned();
        let Some(entry) = entry else {
            return Ok(false);
        };
        let receipt = entry
            .confirmed_stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(receipt) = receipt else {
            return Ok(false);
        };
        if matches!(receipt, RetainedStopReceipt::FailurePending) {
            return Ok(false);
        }
        let attempt: anyhow::Result<bool> = async { Ok(match &receipt {
            RetainedStopReceipt::FailurePending => unreachable!("pending failure is not settled"),
            RetainedStopReceipt::Cancel(cause, reason) => {
                lifecycle.cancel_during_run_owned(*cause, reason).await?
            }
            RetainedStopReceipt::Timeout => lifecycle.timeout().await?,
            RetainedStopReceipt::Failure(outcome) => match outcome {
                crate::tools::call::lifecycle::ToolOutcome::Failed {
                    denial: Some(denial),
                    ..
                } => {
                    lifecycle
                        .fail_with_command_denial(outcome.model_facing_text(), denial)
                        .await?
                }
                crate::tools::call::lifecycle::ToolOutcome::Failed { class, .. } => {
                    let presentation = match self
                        .live_outputs
                        .registry
                        .prepared_presentation(&doc, outcome.model_facing_text())
                        .await
                    {
                        Ok(presentation) => presentation,
                        Err(error) => {
                            tracing::warn!(tool_call_doc_id = %doc, %error, "failed to resolve failed background tool presentation");
                            None
                        }
                    };
                    lifecycle
                        .fail_owned(outcome.model_facing_text(), *class, presentation)
                        .await?
                }
                _ => unreachable!("failure receipt must carry a failed outcome"),
            },
        }) }.await;
        let won = match attempt {
            Ok(won) => won,
            Err(error) => {
                let fresh = ToolCallLifecycle::load_by_doc_id(
                    lifecycle.node().clone(),
                    &doc,
                    lifecycle.node_did(),
                    lifecycle.session_id(),
                    lifecycle.requester_did(),
                )
                .await?;
                match fresh {
                    Some(fresh) if fresh.is_terminal() => {
                        *lifecycle = fresh;
                        false
                    }
                    _ => return Err(error),
                }
            }
        };
        if lifecycle.is_terminal() {
            if won && lifecycle.await_mode() == crate::tools::call::lifecycle::AwaitMode::Background
            {
                let notification = async {
                    let calling_request_id = lifecycle.calling_request_id().await?;
                    let (status, output, reason) = match &receipt {
                        RetainedStopReceipt::FailurePending => {
                            unreachable!("pending failure is not settled")
                        }
                        RetainedStopReceipt::Cancel(_, reason) => {
                            ("cancelled", String::new(), Some(reason.as_str()))
                        }
                        RetainedStopReceipt::Timeout => {
                            ("failed", String::new(), Some("deadline_exceeded"))
                        }
                        RetainedStopReceipt::Failure(_) => {
                            let request_doc = lifecycle.request_doc_id().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "settled tool invocation lacks its physical request"
                                )
                            })?;
                            let raw = crate::tools::background::status::canonical_tool_output(
                                lifecycle.node().as_ref(),
                                &doc,
                                request_doc,
                                lifecycle.session_id(),
                                lifecycle.node_did(),
                                lifecycle.requester_did(),
                            )
                            .await?;
                            ("failed", raw, Some("tool_failed"))
                        }
                    };
                    crate::tools::background::completion::append_background_tool_completion(
                        lifecycle.node(),
                        lifecycle.session_id(),
                        &calling_request_id,
                        &doc,
                        lifecycle.tool_name(),
                        status,
                        &output,
                        reason,
                        crate::request::lifecycle::RequestHopCause::Continuation,
                    )
                    .await
                }
                .await;
                if let Err(error) = notification {
                    tracing::warn!(tool_call_doc_id = %doc, %error, "failed to append recovered background completion");
                }
            }
            self.live_outputs.remove(&doc).await;
            self.retire_confirmed_stop(lifecycle.tool_call_id(), &doc, true);
        }
        Ok(true)
    }

    pub(crate) async fn recover_retained_foreground(&self) {
        let pending = self
            .lock_executions()
            .values()
            .filter_map(|execution| {
                let lifecycle = execution
                    .recovery_lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()?;
                Some((
                    execution.tool_call_doc_id.clone(),
                    execution.clone(),
                    lifecycle,
                ))
            })
            .collect::<Vec<_>>();
        for (doc, execution, mut lifecycle) in pending {
            if execution
                .confirmed_stop
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
            {
                let observed_exit = self
                    .process_record(lifecycle.tool_call_id(), &doc)
                    .is_some_and(|record| {
                        record.identity.observe()
                            == crate::tools::exec::ownership::ProcessObservation::Exited
                    });
                #[cfg(windows)]
                let observed_exit = observed_exit
                    || self.stop_failed_job(lifecycle.tool_call_id(), &doc).await == Some(true);
                if observed_exit {
                    execution.process_evidence.exited();
                    self.remember_confirmed_stop(&execution);
                }
            }
            let stop = execution
                .confirmed_stop
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if stop.is_some() {
                if let Err(error) = self.settle_retained_stop(&mut lifecycle).await {
                    tracing::warn!(tool_call_doc_id = %doc, %error, "confirmed process stop settlement remains pending");
                }
                continue;
            }
            if !execution
                .handoff_pending
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                continue;
            }
            let receipt = serde_json::json!({"tool_call_id": lifecycle.tool_call_id(), "status": "running", "await_mode": "background", "message": "Process exit could not be confirmed; the call remains available for control."}).to_string();
            match lifecycle.background_with_recovery_receipt(&receipt).await {
                Ok(()) => {
                    *execution
                        .recovery_lifecycle
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lifecycle);
                    execution
                        .handoff_pending
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }
                Err(error) => {
                    tracing::warn!(tool_call_doc_id = %doc, %error, "retained foreground process handoff remains pending")
                }
            }
        }
    }

    pub async fn wait_for_completion(&self, tool_call_id: &str) {
        let receivers = self
            .lock_executions()
            .values()
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .map(|execution| execution.completion_tx.subscribe())
            .collect::<Vec<_>>();
        for receiver in receivers {
            Self::await_completion(receiver).await;
        }
    }

    async fn await_completion(mut receiver: watch::Receiver<bool>) {
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }

    fn remove_now(&self, tool_call_doc_id: &str) {
        if let Some(execution) = self.lock_executions().remove(tool_call_doc_id) {
            execution.worker_release_tx.send_replace(true);
            execution.completion_tx.send_replace(true);
        }
    }

    fn lock_executions(
        &self,
    ) -> std::sync::MutexGuard<'_, RapidHashMap<String, BackgroundExecution>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Cancellation-safe ownership reservation for the durable-running handoff.
/// Dropping the caller future before `tokio::spawn` transfers ownership removes
/// the volatile entry synchronously, so periodic recovery can see the orphan.
pub(crate) struct BackgroundExecutionReservation {
    registry: BackgroundExecutionRegistry,
    tool_call_doc_id: String,
    armed: bool,
    _service_work: Arc<crate::runtime_service::RuntimeServiceWork>,
}

impl BackgroundExecutionReservation {
    fn retain_for_recovery(self, lifecycle: ToolCallLifecycle) {
        self.retain_stopped_worker(lifecycle, true);
    }

    fn retain_stopped_worker(mut self, lifecycle: ToolCallLifecycle, handoff_pending: bool) {
        if let Some(execution) = self
            .registry
            .lock_executions()
            .get(&self.tool_call_doc_id)
            .cloned()
        {
            *execution
                .recovery_lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lifecycle);
            execution
                .handoff_pending
                .store(handoff_pending, std::sync::atomic::Ordering::SeqCst);
            execution
                .retained
                .store(true, std::sync::atomic::Ordering::SeqCst);
            execution.worker_release_tx.send_replace(true);
            self.armed = false;
        }
    }

    #[cfg(test)]
    pub(crate) fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for BackgroundExecutionReservation {
    fn drop(&mut self) {
        if self.armed {
            self.registry.remove_now(&self.tool_call_doc_id);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptTurnState {
    Idle,
    AssistantBuilding { sequence: u32 },
}

#[derive(Debug, Clone, Default)]
struct ToolResultIdentity {
    result_id: Option<String>,
    call_id: Option<String>,
}

struct SessionState {
    session_id: Option<String>,
    current_request_id: Option<String>,
    current_request_doc_id: Option<String>,
    current_requester_did: Option<String>,
    request_deadline_at: Option<DateTime<Utc>>,
    sequence: u32,
    transcript_turn: TranscriptTurnState,
    tool_result_identities: RapidHashMap<String, ToolResultIdentity>,
}

impl SessionState {
    // Tool-result messages stay in the assistant turn so parallel calls share
    // its sequence (Lean: `Transcript.parallel_results_complete_independently`).
    fn reset_after_user_message(&mut self) {
        self.transcript_turn = TranscriptTurnState::Idle;
    }

    fn begin_or_continue_assistant_turn(&mut self) -> u32 {
        match self.transcript_turn {
            TranscriptTurnState::AssistantBuilding { sequence } => sequence,
            TranscriptTurnState::Idle => {
                self.sequence += 1;
                let sequence = self.sequence;
                self.transcript_turn = TranscriptTurnState::AssistantBuilding { sequence };
                sequence
            }
        }
    }

    fn register_tool_result_identity(
        &mut self,
        internal_call_id: &str,
        result_id: Option<&str>,
        call_id: Option<&str>,
    ) {
        let identity = self
            .tool_result_identities
            .entry(internal_call_id.to_string())
            .or_default();
        if let Some(result_id) = non_empty(result_id) {
            identity.result_id = Some(result_id.to_string());
        }
        if let Some(call_id) = non_empty(call_id) {
            identity.call_id = Some(call_id.to_string());
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| (!value.is_empty()).then_some(value))
}

#[derive(Clone)]
pub struct DefraSessionHook {
    node: Arc<EmbeddedNode>,
    node_did: String,
    truncation_limits: TruncationLimits,
    failure_policy: FailurePolicy,
    counters: Arc<HookCounters>,
    state: Arc<Mutex<SessionState>>,
    in_flight_lifecycles: Arc<Mutex<RapidHashMap<String, ToolCallLifecycle>>>,
    foreground_process_evidence: Arc<
        Mutex<RapidHashMap<String, Arc<crate::tools::exec::ownership::LiveProcessEvidenceOwner>>>,
    >,
    accepted_tool_calls:
        Arc<Mutex<RapidHashMap<String, crate::request::streaming::AcceptedToolCall>>>,
    effect_tool_calls: Arc<Mutex<RapidHashMap<String, (ToolCallLifecycle, String)>>>,
    effect_lifecycles: Arc<Mutex<RapidHashMap<String, ToolCallLifecycle>>>,
    background_tool_registry: BackgroundToolRegistry,
    background_executions: BackgroundExecutionRegistry,
    background_live_outputs: BackgroundLiveOutputState,
    operator_tool_root: Option<PathBuf>,
    remote_tools: Option<crate::documents::RemoteTools>,
    goal_tools_enabled: bool,
    goal_creation_enabled: bool,
    job_tools: RapidHashMap<String, crate::tools::background::job::JobBinding>,
    /// The runtime's job driver, whose handles step 0 of a job tool runs on.
    pub(crate) job_driver: Option<Arc<crate::tools::background::job::driver::JobDriver>>,
    output_obligation_gate: Option<crate::agent::output_obligation::OutputObligationGate>,
    steering_input_source: Option<Arc<SteeringInputSource>>,
    inference_observation:
        Option<std::sync::Weak<crate::inference_observation::InferenceObservationGuard>>,
}

struct SteeringInputSource {
    request: crate::request::watcher::AgentRequest,
    verifier: crate::request::admission::AgentRequestAdmissionVerifier,
    natural_finish_allowed: bool,
}

enum PolicyDecision {
    Continue,
    Terminate(String),
}

impl DefraSessionHook {
    pub(crate) fn with_inference_observation(
        mut self,
        observation: Option<Arc<crate::inference_observation::InferenceObservationGuard>>,
    ) -> Self {
        self.inference_observation = observation.as_ref().map(Arc::downgrade);
        self
    }

    pub(crate) fn with_steering_inputs(
        mut self,
        request: crate::request::watcher::AgentRequest,
        verifier: crate::request::admission::AgentRequestAdmissionVerifier,
        natural_finish_allowed: bool,
    ) -> Self {
        self.steering_input_source = Some(Arc::new(SteeringInputSource {
            request,
            verifier,
            natural_finish_allowed,
        }));
        self
    }
    pub(crate) async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, crate::request::streaming::AcceptedToolCall)>,
    ) -> anyhow::Result<()> {
        let mut pending = self.accepted_tool_calls.lock().await;
        for (internal_id, call) in calls {
            anyhow::ensure!(
                pending.insert(internal_id.clone(), call).is_none(),
                "duplicate accepted tool binding for internal call {internal_id}"
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn adopt_accepted_tool_dispatch(
        &self,
        internal_call_id: &str,
        provider_call_id: Option<&str>,
        request_id: &str,
        session_id: &str,
        tool_name: &str,
        args: &str,
        deadline_at: DateTime<Utc>,
        await_mode: AwaitMode,
    ) -> anyhow::Result<ToolCallLifecycle> {
        if let Some((lifecycle, recorded_arguments)) =
            self.effect_tool_calls.lock().await.remove(internal_call_id)
        {
            anyhow::ensure!(
                provider_call_id.is_none()
                    && lifecycle.tool_name() == tool_name
                    && lifecycle.request_id() == request_id
                    && lifecycle.session_id() == session_id
                    && recorded_arguments == args,
                "plugin effect dispatch does not match its admitted input"
            );
            return Ok(lifecycle);
        }
        let registered = self
            .state
            .lock()
            .await
            .tool_result_identities
            .get(internal_call_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "tool dispatch lacks StreamProcessor native identity for internal call {internal_call_id}"
                )
            })?;
        let native_id = registered.result_id.ok_or_else(|| {
            anyhow::anyhow!(
                "tool dispatch lacks registered provider tool id for internal call {internal_call_id}"
            )
        })?;
        if let Some(provider_call_id) = provider_call_id {
            anyhow::ensure!(
                Some(provider_call_id) == registered.call_id.as_deref(),
                "dispatch provider call id does not match StreamProcessor identity"
            );
        }

        let request_doc_id = self
            .active_request_doc_id_for(request_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("accepted dispatch requires a physical request binding")
            })?;
        let accepted = self
            .accepted_tool_calls
            .lock()
            .await
            .remove(internal_call_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "tool dispatch lacks its provider-published AcceptedToolCall binding"
                )
            })?;
        anyhow::ensure!(
            accepted.id == native_id
                && accepted.call_id == registered.call_id
                && accepted.request_doc_id == request_doc_id
                && accepted.session_id == session_id
                && accepted.tool_name == tool_name,
            "accepted tool binding does not match registered provider dispatch identity"
        );
        let selected = self
            .remote_tools
            .as_ref()
            .and_then(|remote| selected_remote_identity(tool_name, args, remote));
        Ok(ToolCallLifecycle::from_accepted(
            self.node.clone(),
            self.node_did.clone(),
            self.active_requester_did().await,
            accepted,
            deadline_at,
            await_mode,
        )?
        .with_selected_tool_identity(selected))
    }

    #[cfg(test)]
    pub fn with_identity(
        node: Arc<EmbeddedNode>,
        node_did: &str,
        failure_policy: FailurePolicy,
    ) -> Self {
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();
        Self {
            node,
            node_did: node_did.to_string(),
            truncation_limits: TruncationLimits::default(),
            failure_policy,
            counters: Arc::new(HookCounters {
                failures: AtomicU64::new(0),
                successes: AtomicU64::new(0),
            }),
            state: Arc::new(Mutex::new(SessionState {
                session_id: Some(uuid::Uuid::new_v4().to_string()),
                current_request_id: None,
                current_request_doc_id: None,
                current_requester_did: None,
                request_deadline_at: None,
                sequence: 0,
                transcript_turn: TranscriptTurnState::Idle,
                tool_result_identities: RapidHashMap::new(),
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            foreground_process_evidence: Arc::new(Mutex::new(RapidHashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            job_tools: RapidHashMap::new(),
            job_driver: None,
            output_obligation_gate: None,
            steering_input_source: None,
            inference_observation: None,
        }
    }

    pub async fn resume_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        session::require_session(&node, node_did, session_id, requester_did).await?;
        let max_seq = session::max_sequence(&node, session_id, node_did, requester_did).await?;
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();

        Ok(Self {
            node,
            node_did: node_did.to_string(),
            truncation_limits: TruncationLimits::default(),
            failure_policy,
            counters: Arc::new(HookCounters {
                failures: AtomicU64::new(0),
                successes: AtomicU64::new(0),
            }),
            state: Arc::new(Mutex::new(SessionState {
                session_id: Some(session_id.to_string()),
                current_request_id: None,
                current_request_doc_id: None,
                current_requester_did: None,
                request_deadline_at: None,
                sequence: max_seq,
                transcript_turn: TranscriptTurnState::Idle,
                tool_result_identities: RapidHashMap::new(),
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            foreground_process_evidence: Arc::new(Mutex::new(RapidHashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            job_tools: RapidHashMap::new(),
            job_driver: None,
            output_obligation_gate: None,
            steering_input_source: None,
            inference_observation: None,
        })
    }

    pub fn with_background_tool_registry(mut self, registry: BackgroundToolRegistry) -> Self {
        self.background_tool_registry = registry;
        self
    }

    pub fn with_background_execution_registry(
        mut self,
        registry: BackgroundExecutionRegistry,
    ) -> Self {
        self.background_live_outputs = registry.live_outputs.clone();
        self.background_executions = registry;
        self
    }

    pub fn with_operator_tool_root(mut self, root: Option<PathBuf>) -> Self {
        self.operator_tool_root = root;
        self
    }

    pub fn with_goal_tool_authority(
        mut self,
        goal_tools_enabled: bool,
        goal_creation_enabled: bool,
    ) -> Self {
        self.goal_tools_enabled = goal_tools_enabled;
        self.goal_creation_enabled = goal_tools_enabled && goal_creation_enabled;
        self
    }

    /// Bind the Agent's job tools; a plugin child call naming one is refused.
    pub(crate) fn with_job_tools(
        mut self,
        job_tools: RapidHashMap<String, crate::tools::background::job::JobBinding>,
    ) -> Self {
        self.job_tools = job_tools;
        self
    }

    /// Bind the runtime's job driver; a job tool is refused without it.
    pub(crate) fn with_job_driver(
        mut self,
        driver: Arc<crate::tools::background::job::driver::JobDriver>,
    ) -> Self {
        self.job_driver = Some(driver);
        self
    }

    pub(crate) fn with_output_obligation_gate(
        mut self,
        gate: Option<crate::agent::output_obligation::OutputObligationGate>,
    ) -> Self {
        self.output_obligation_gate = gate;
        self
    }

    pub fn set_operator_tool_root(&mut self, root: Option<PathBuf>) {
        self.operator_tool_root = root;
    }

    pub fn stats(&self) -> HookStats {
        HookStats {
            persistence_failures: self.counters.failures.load(Ordering::Relaxed),
            persistence_successes: self.counters.successes.load(Ordering::Relaxed),
        }
    }

    fn record_success(&self) {
        self.counters.successes.fetch_add(1, Ordering::Relaxed);
    }

    fn decide_persistence_outcome(&self, context: &str, error: &anyhow::Error) -> PolicyDecision {
        decide_persistence_outcome(self.failure_policy, &self.counters, context, error)
    }

    fn on_persistence_error(&self, context: &str, error: &anyhow::Error) -> HookAction {
        match self.decide_persistence_outcome(context, error) {
            PolicyDecision::Continue => HookAction::Continue,
            PolicyDecision::Terminate(reason) => HookAction::Terminate { reason },
        }
    }

    fn on_tool_persistence_error(
        &self,
        context: &str,
        error: &anyhow::Error,
    ) -> ToolCallHookAction {
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        tracing::error!(error = %error, context = %context, "tool call persistence failed; dispatch denied");
        ToolCallHookAction::Terminate {
            reason: format!("tool call persistence failed: {error}"),
        }
    }

    pub async fn session_id(&self) -> Option<String> {
        self.state.lock().await.session_id.clone()
    }

    pub async fn set_active_request_lineage(
        &self,
        request_id: Option<String>,
        requester_did: Option<String>,
    ) -> anyhow::Result<()> {
        let request_doc_id = match request_id.as_deref() {
            Some(request_id) => Some(
                self.request_doc_id_for_request(request_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("active AgentRequest {request_id} not found"))?,
            ),
            None => None,
        };
        self.set_active_request_binding(request_id, request_doc_id, requester_did)
            .await;
        Ok(())
    }

    pub async fn set_active_request_binding(
        &self,
        request_id: Option<String>,
        request_doc_id: Option<String>,
        requester_did: Option<String>,
    ) {
        if request_id.is_some() != request_doc_id.is_some() {
            tracing::warn!(
                has_request_id = request_id.is_some(),
                has_request_doc_id = request_doc_id.is_some(),
                "rejected half-bound active request lineage",
            );
            let mut state = self.state.lock().await;
            state.current_request_id = None;
            state.current_request_doc_id = None;
            state.current_requester_did = None;
            return;
        }
        let mut state = self.state.lock().await;
        state.current_request_id = request_id;
        state.current_request_doc_id = request_doc_id;
        state.current_requester_did = requester_did;
    }

    async fn active_requester_did(&self) -> Option<String> {
        self.state.lock().await.current_requester_did.clone()
    }

    #[cfg(test)]
    async fn active_request_doc_id(&self) -> Option<String> {
        self.state.lock().await.current_request_doc_id.clone()
    }

    async fn active_request_doc_id_for(&self, request_id: &str) -> anyhow::Result<Option<String>> {
        if request_id.trim().is_empty() {
            return Ok(None);
        }
        let state = self.state.lock().await;
        if state.current_request_id.as_deref() != Some(request_id) {
            anyhow::bail!("active request changed while resolving document provenance");
        }
        let doc_id = state
            .current_request_doc_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("active request is missing its physical document id"))?;
        Ok(Some(doc_id))
    }

    async fn request_doc_id_for_request(&self, request_id: &str) -> anyhow::Result<Option<String>> {
        crate::request::binding::resolve_request_doc_id(self.node.as_ref(), request_id).await
    }

    pub(crate) async fn register_stream_tool_call_identity(
        &self,
        internal_call_id: &str,
        result_id: &str,
        call_id: Option<&str>,
    ) {
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            Some(result_id),
            call_id,
        );
    }

    pub async fn set_request_deadline_at(&self, deadline_at: Option<DateTime<Utc>>) {
        self.state.lock().await.request_deadline_at = deadline_at;
    }

    pub(crate) fn with_remote_tools(
        mut self,
        tools: Option<crate::documents::RemoteTools>,
    ) -> Self {
        self.remote_tools = tools;
        self
    }

    pub(crate) async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> crate::tools::background::status::LiveToolOutputWriter {
        let binding = self
            .in_flight_lifecycles
            .lock()
            .await
            .get(internal_call_id)
            .and_then(|lifecycle| lifecycle.tool_output_binding().ok());
        if let Some(binding) = binding {
            return self
                .background_live_outputs
                .canonical_writer_for(binding)
                .await;
        }
        self.background_live_outputs
            .writer_for(internal_call_id)
            .await
    }

    pub(crate) async fn release_live_output(&self, tool_call_id: &str) {
        self.background_live_outputs.remove(tool_call_id).await;
    }

    async fn foreground_stop_confirmed(
        &self,
        lifecycle: &ToolCallLifecycle,
        receipt: RetainedStopReceipt,
    ) -> bool {
        if !crate::tools::native::is_bash_tool_name(lifecycle.tool_name()) {
            return true;
        }
        let Some(doc) = lifecycle.doc_id() else {
            return false;
        };
        let evidence = self
            .foreground_process_evidence
            .lock()
            .await
            .get(doc)
            .cloned();
        let Some(evidence) = evidence else {
            return true;
        };
        self.background_executions
            .set_stop_receipt(lifecycle.tool_call_id(), doc, receipt);
        match self
            .background_executions
            .stop_native_process(lifecycle.tool_call_id(), doc)
            .await
        {
            crate::tools::exec::ProcessStopOutcome::Stopped
            | crate::tools::exec::ProcessStopOutcome::AlreadyExited => true,
            crate::tools::exec::ProcessStopOutcome::StillRunning => false,
            crate::tools::exec::ProcessStopOutcome::NotOwned => !matches!(
                evidence.get(),
                crate::tools::exec::ownership::LiveProcessEvidence::SpawnedUnverified
            ),
        }
    }

    async fn forget_foreground_evidence(&self, lifecycle: &ToolCallLifecycle) {
        if let Some(doc) = lifecycle.doc_id() {
            self.foreground_process_evidence.lock().await.remove(doc);
        }
    }

    pub(crate) async fn timeout_expired_tool_calls(&self) -> anyhow::Result<usize> {
        let lifecycles = {
            let now = Utc::now();
            let mut map = self.in_flight_lifecycles.lock().await;
            let expired_ids = map
                .iter()
                .filter(|&(_, lifecycle)| lifecycle.is_deadline_expired(now))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();

            expired_ids
                .into_iter()
                .filter_map(|id| map.remove(&id).map(|lifecycle| (id, lifecycle)))
                .collect::<Vec<_>>()
        };

        let mut count = 0;
        for (id, mut lifecycle) in lifecycles {
            if !self
                .foreground_stop_confirmed(&lifecycle, RetainedStopReceipt::Timeout)
                .await
            {
                let still_foreground = match lifecycle.doc_id() {
                    Some(doc) => self
                        .foreground_process_evidence
                        .lock()
                        .await
                        .contains_key(doc),
                    None => true,
                };
                if still_foreground {
                    self.in_flight_lifecycles.lock().await.insert(id, lifecycle);
                }
                continue;
            }
            if !self
                .background_executions
                .settle_retained_stop(&mut lifecycle)
                .await?
            {
                let _ = lifecycle.timeout().await?;
            }
            if lifecycle.is_terminal() {
                if let Some(doc) = lifecycle.doc_id() {
                    self.background_executions.retire_confirmed_stop(
                        lifecycle.tool_call_id(),
                        doc,
                        true,
                    );
                }
            }
            self.forget_foreground_evidence(&lifecycle).await;
            count += 1;
        }
        let effects = self
            .effect_lifecycles
            .lock()
            .await
            .iter()
            .filter(|(_, child)| child.is_deadline_expired(Utc::now()))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in effects {
            <Self as gents_loop::session_hook::SessionHook>::finish_tool_effect(
                self,
                &id,
                Some(&gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { deadline_at: None }),
            )
            .await
            .map_err(anyhow::Error::msg)?;
        }
        Ok(count)
    }

    pub async fn cancel_in_flight_tool_calls(&self) -> anyhow::Result<usize> {
        let lifecycles = {
            let mut map = self.in_flight_lifecycles.lock().await;
            map.drain().collect::<Vec<_>>()
        };

        let mut cancelled = 0;
        let mut first_error = None;
        for (id, mut lifecycle) in lifecycles {
            if lifecycle.is_running()
                && matches!(
                    lifecycle.interrupt_disposition(),
                    InterruptDisposition::Cancel
                )
                && !self
                    .foreground_stop_confirmed(
                        &lifecycle,
                        RetainedStopReceipt::Cancel(
                            CancelCause::Interrupted,
                            "request interrupted".to_owned(),
                        ),
                    )
                    .await
            {
                let still_foreground = match lifecycle.doc_id() {
                    Some(doc) => self
                        .foreground_process_evidence
                        .lock()
                        .await
                        .contains_key(doc),
                    None => true,
                };
                if still_foreground {
                    self.in_flight_lifecycles.lock().await.insert(id, lifecycle);
                }
                continue;
            }
            let applied = match lifecycle.interrupt_disposition() {
                InterruptDisposition::Cancel if lifecycle.is_running() => {
                    match self
                        .background_executions
                        .settle_retained_stop(&mut lifecycle)
                        .await
                    {
                        Ok(true) => {
                            cancelled += usize::from(lifecycle.is_cancelled());
                            Ok(())
                        }
                        Ok(false) => lifecycle
                            .cancel_during_run(CancelCause::Interrupted)
                            .await
                            .map(|won| cancelled += usize::from(won)),
                        Err(error) => Err(error),
                    }
                }
                InterruptDisposition::Cancel | InterruptDisposition::Retain => Ok(()),
            };
            if let Err(error) = applied {
                first_error.get_or_insert(error);
            } else {
                if lifecycle.is_terminal() {
                    if let Some(doc) = lifecycle.doc_id() {
                        self.background_executions.retire_confirmed_stop(
                            lifecycle.tool_call_id(),
                            doc,
                            true,
                        );
                    }
                }
                self.forget_foreground_evidence(&lifecycle).await;
            }
        }
        let effects = self
            .effect_lifecycles
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in effects {
            if let Err(error) = <Self as gents_loop::session_hook::SessionHook>::finish_tool_effect(
                self,
                &id,
                Some(&gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled),
            )
            .await
            {
                first_error.get_or_insert(anyhow::Error::msg(error));
            }
        }
        first_error.map_or(Ok(cancelled), Err)
    }

    #[cfg(test)]
    pub(crate) async fn fail_in_flight_tool_calls(
        &self,
        result: &str,
        failure_class: crate::tools::call::lifecycle::FailureClass,
    ) -> anyhow::Result<usize> {
        let lifecycles = {
            let mut map = self.in_flight_lifecycles.lock().await;
            map.drain()
                .map(|(_, lifecycle)| lifecycle)
                .collect::<Vec<_>>()
        };

        let count = lifecycles.len();
        for mut lifecycle in lifecycles {
            lifecycle.fail(result, failure_class).await?;
        }
        Ok(count)
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        let session_id = self.state.lock().await.session_id.clone();
        if let Some(id) = session_id {
            session::close_session(
                &self.node,
                &self.node_did,
                &id,
                self.active_requester_did().await.as_deref(),
            )
            .await?;
        }
        Ok(())
    }

    pub fn apply_persistence_policy(
        &self,
        result: anyhow::Result<()>,
        context: &str,
    ) -> anyhow::Result<()> {
        match result {
            Ok(()) => {
                self.record_success();
                Ok(())
            }
            Err(e) => match self.decide_persistence_outcome(context, &e) {
                PolicyDecision::Continue => Ok(()),
                PolicyDecision::Terminate(_) => Err(e),
            },
        }
    }
}

impl DefraSessionHook {
    async fn ensure_steering_before_deadline(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            crate::request::lifecycle::RequestLifecycle::input_publication_before_deadline(
                Utc::now(),
                self.state.lock().await.request_deadline_at,
            ),
            "request deadline expired before steering intake"
        );
        Ok(())
    }
}

#[async_trait::async_trait]
impl gents_loop::session_hook::CanonicalSessionHook<crate::request::streaming::AcceptedToolCall>
    for DefraSessionHook
{
    async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, crate::request::streaming::AcceptedToolCall)>,
    ) -> Result<(), Self::Error> {
        Ok(DefraSessionHook::adopt_accepted_tool_calls(self, calls).await?)
    }
}

#[async_trait::async_trait]
impl gents_loop::session_hook::SessionHook for DefraSessionHook {
    type Error = crate::agent::stream_processor::RuntimeSeamError;
    fn observe_provider(&self, observation: gents_loop::session_hook::ProviderObservation) {
        if let Some(owner) = self
            .inference_observation
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            owner.observe(observation);
        }
    }

    async fn run_foreground_tool(
        &self,
        name: &str,
        internal_id: &str,
        call: gents_loop::session_hook::ForegroundCall,
    ) -> gents_loop::session_hook::ForegroundResult {
        let _activity = self
            .inference_observation
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .map(|owner| owner.begin_tool(internal_id));
        self.run_backgroundable_foreground(name, internal_id, call)
            .await
    }

    async fn admit_tool_effect(
        &self,
        parent_internal_id: &str,
        ordinal: u32,
        tool_name: &str,
        arguments: &str,
    ) -> Result<String, String> {
        let parents = self.in_flight_lifecycles.lock().await;
        let parent = parents
            .get(parent_internal_id)
            .cloned()
            .ok_or_else(|| "plugin effect has no running parent".to_owned())?;
        drop(parents);
        let mut child = parent
            .admit_plugin_effect(ordinal, tool_name, arguments)
            .await
            .map_err(|error| format!("cannot admit plugin effect: {error:#}"))?;
        let internal_id = child.plugin_effect_internal_id().to_owned();
        if !crate::plugin::tool_calls::supported(
            tool_name,
            &self
                .job_tools
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<String>>(),
        ) {
            let reason = format!(
                "{tool_name} controls request, session or process lifecycle; call it directly outside the plugin"
            );
            child
                .spawn_failed(
                    crate::tools::call::lifecycle::FailureClass::PolicyDenied,
                    &reason,
                )
                .await
                .map_err(|error| format!("cannot persist plugin effect refusal: {error:#}"))?;
            return Err(reason);
        }
        let mut pending = self.effect_tool_calls.lock().await;
        if pending.contains_key(&internal_id) {
            return Err("plugin effect is already awaiting dispatch".into());
        }
        self.effect_lifecycles
            .lock()
            .await
            .insert(internal_id.clone(), child.clone());
        pending.insert(internal_id.clone(), (child, arguments.to_owned()));
        Ok(internal_id)
    }

    async fn finish_tool_effect(
        &self,
        internal_id: &str,
        interruption: Option<&gents_loop::tool_call_lifecycle::ToolOutcome>,
    ) -> Result<(), String> {
        let Some(binding) = self
            .effect_lifecycles
            .lock()
            .await
            .get(internal_id)
            .cloned()
        else {
            return Ok(());
        };
        self.effect_tool_calls.lock().await.remove(internal_id);
        let result: anyhow::Result<()> = async {
            let mut current = ToolCallLifecycle::load_by_doc_id(
                self.node.clone(),
                binding
                    .doc_id()
                    .ok_or_else(|| anyhow::anyhow!("plugin child has no physical identity"))?,
                binding.node_did(),
                binding.session_id(),
                binding.requester_did(),
            )
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("plugin child disappeared before interruption settlement")
            })?;
            let Some(outcome) = interruption else {
                anyhow::ensure!(
                    current.is_terminal(),
                    "plugin child did not durably terminalize"
                );
                crate::tools::call::lifecycle::query::load_tool_call_result(
                    &crate::store::config_client::ConfigAccess::local(self.node.clone()),
                    current.doc_id().expect("loaded physical child"),
                    current.node_did(),
                    current.session_id(),
                    current.requester_did(),
                )
                .await?;
                self.in_flight_lifecycles.lock().await.remove(internal_id);
                return Ok(());
            };
            let cause = match outcome {
                gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. } => {
                    crate::tools::call::lifecycle::CancelCause::Deadline
                }
                _ => crate::tools::call::lifecycle::CancelCause::Interrupted,
            };
            match current.state() {
                crate::tools::call::lifecycle::ToolCallState::Pending => {
                    current.cancel_before_dispatch(cause).await?
                }
                crate::tools::call::lifecycle::ToolCallState::Running => {
                    if cause == crate::tools::call::lifecycle::CancelCause::Deadline {
                        current.timeout().await?;
                    } else {
                        current.cancel_during_run(cause).await?;
                    }
                }
                _ => {}
            }
            self.in_flight_lifecycles.lock().await.remove(internal_id);
            Ok(())
        }
        .await;
        if result.is_ok() {
            self.effect_lifecycles.lock().await.remove(internal_id);
        }
        result.map_err(|error| format!("cannot settle plugin effect: {error:#}"))
    }

    async fn pending_steering_inputs(
        &self,
    ) -> Result<Vec<gents_loop::loop_stream::FoldedPrompt>, Self::Error> {
        let Some(source) = self.steering_input_source.as_ref() else {
            return Ok(Vec::new());
        };
        self.ensure_steering_before_deadline().await?;
        Ok(crate::request::lifecycle::queue::steering_inputs(
            &self.node,
            &source.request,
            &source.verifier,
        )
        .await?)
    }

    async fn finish_steering_snapshot(
        &self,
    ) -> Result<Option<gents_loop::loop_stream::SteeringSnapshot>, Self::Error> {
        let Some(source) = self
            .steering_input_source
            .as_ref()
            .filter(|source| source.natural_finish_allowed)
        else {
            return Ok(None);
        };
        self.ensure_steering_before_deadline().await?;
        Ok(Some(
            crate::request::lifecycle::queue::steering_snapshot(
                &self.node,
                &source.request,
                &source.verifier,
            )
            .await?,
        ))
    }

    async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        history: &[Message],
        context: Option<&Message>,
    ) -> HookAction {
        DefraSessionHook::on_completion_call_with_context(self, prompt, history, context).await
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        DefraSessionHook::on_tool_call(self, tool_name, tool_call_id, internal_call_id, args).await
    }

    async fn on_tool_admission_rejected(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &gents_loop::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        DefraSessionHook::on_tool_admission_rejected(
            self,
            tool_name,
            tool_call_id,
            internal_call_id,
            args,
            outcome,
        )
        .await
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &gents_loop::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        DefraSessionHook::on_tool_result(
            self,
            tool_name,
            tool_call_id,
            internal_call_id,
            args,
            outcome,
        )
        .await
    }

    async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> gents_loop::live_output::LiveToolOutputWriter {
        DefraSessionHook::foreground_live_output_writer(self, internal_call_id).await
    }

    async fn session_id(&self) -> Option<String> {
        DefraSessionHook::session_id(self).await
    }

    async fn register_stream_tool_call_identity(
        &self,
        internal_call_id: &str,
        result_id: &str,
        call_id: Option<&str>,
    ) {
        DefraSessionHook::register_stream_tool_call_identity(
            self,
            internal_call_id,
            result_id,
            call_id,
        )
        .await
    }
}

fn decide_persistence_outcome(
    failure_policy: FailurePolicy,
    counters: &HookCounters,
    context: &str,
    error: &anyhow::Error,
) -> PolicyDecision {
    counters.failures.fetch_add(1, Ordering::Relaxed);
    match failure_policy {
        FailurePolicy::FailOpen => {
            tracing::warn!(error = %error, context = %context, "persistence failed (fail-open)");
            PolicyDecision::Continue
        }
        FailurePolicy::FailClosed => {
            tracing::error!(error = %error, context = %context, "persistence failed (fail-closed) — terminating");
            PolicyDecision::Terminate(format!("persistence failed: {error}"))
        }
    }
}
