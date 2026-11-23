use rapidhash::RapidHashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::call::lifecycle::ToolCallLifecycle;
use gents_loop::live_output::LiveToolOutputRegistry;
use gents_protocol::tool_call::CancelCause;

#[derive(Clone)]
pub(super) enum RetainedStopReceipt {
    Cancel(CancelCause, String),
    Timeout,
    Failure(gents_loop::tool_call_lifecycle::ToolOutcome),
    FailurePending,
}

impl RetainedStopReceipt {
    pub(super) fn from_outcome(outcome: &gents_loop::tool_call_lifecycle::ToolOutcome) -> Self {
        match outcome {
            gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled => {
                Self::Cancel(CancelCause::Interrupted, "request interrupted".to_owned())
            }
            gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. } => Self::Timeout,
            gents_loop::tool_call_lifecycle::ToolOutcome::Failed { .. } => {
                Self::Failure(outcome.clone())
            }
            gents_loop::tool_call_lifecycle::ToolOutcome::Completed(_) => {
                unreachable!("completion does not abandon a native future")
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct BackgroundExecution {
    tool_call_id: String,
    cancellation_token: CancellationToken,
    pub(super) completion_tx: watch::Sender<bool>,
    pub(super) worker_release_tx: watch::Sender<bool>,
    tool_call_doc_id: String,
    pub(super) process_evidence: Arc<gents_session::exec::ownership::LiveProcessEvidenceOwner>,
    process_identity:
        Arc<std::sync::Mutex<Option<gents_session::exec::ownership::ProcessIdentity>>>,
    cancellation_reason: Arc<std::sync::Mutex<Option<String>>>,
    pub(super) recovery_lifecycle: Arc<std::sync::Mutex<Option<ToolCallLifecycle>>>,
    pub(super) retained: Arc<std::sync::atomic::AtomicBool>,
    handoff_pending: Arc<std::sync::atomic::AtomicBool>,
    pub(super) requested_stop: Arc<std::sync::Mutex<Option<RetainedStopReceipt>>>,
    confirmed_stop: Arc<std::sync::Mutex<Option<RetainedStopReceipt>>>,
    _service_work: Arc<gents_store::service::RuntimeServiceWork>,
}

#[derive(Clone, Default)]
pub(super) struct BackgroundLiveOutputState {
    pub(super) registry: LiveToolOutputRegistry,
}

impl BackgroundLiveOutputState {
    pub(super) async fn writer_for(
        &self,
        tool_call_id: impl Into<String>,
    ) -> gents_loop::live_output::LiveToolOutputWriter {
        self.registry.writer_for(tool_call_id).await
    }

    pub(super) async fn canonical_writer_for(
        &self,
        binding: crate::call::lifecycle::delivery::ToolOutputBinding,
    ) -> gents_loop::live_output::LiveToolOutputWriter {
        let tool_call_doc_id = binding.tool_call_doc_id.clone();
        self.registry
            .canonical_writer_for(tool_call_doc_id, Arc::new(binding))
            .await
    }

    pub(super) async fn remove(&self, tool_call_id: &str) {
        self.registry.remove(tool_call_id).await;
    }
}

/// The hosting runtime's execution owners as session resume's quiescence
/// witnesses.
pub struct RuntimeExecutionWitness {
    pub background_executions: BackgroundExecutionRegistry,
    pub job_driver: Arc<crate::background::job::driver::JobDriver>,
}

impl gents_session::session::SessionExecutionWitness for RuntimeExecutionWitness {
    fn active_tool_call_doc_ids(&self) -> Vec<String> {
        self.background_executions.active_tool_call_doc_ids()
    }

    fn process_records(
        &self,
    ) -> anyhow::Result<Vec<gents_session::exec::ownership::ProcessRecord>> {
        self.background_executions.try_process_record_list()
    }

    fn in_flight_job_row_ids(&self) -> Vec<String> {
        self.job_driver.in_flight_row_ids()
    }
}

#[derive(Clone, Default)]
pub struct BackgroundExecutionRegistry {
    inner: Arc<std::sync::Mutex<RapidHashMap<String, BackgroundExecution>>>,
    pub(super) live_outputs: BackgroundLiveOutputState,
    pub(super) process_records: gents_session::exec::ownership::ProcessRecordStore,
    // Recovery revisits a retained record on every sweep, so a group that
    // outlives its leader warns once per record and then logs at debug.
    pub(super) unproven_record_warnings: Arc<std::sync::Mutex<rapidhash::RapidHashSet<String>>>,
    #[cfg(windows)]
    failed_jobs: Arc<
        std::sync::Mutex<RapidHashMap<String, (String, Arc<gents_session::exec::FailedJobOwner>)>>,
    >,
    task_hook_records: crate::call::task_hooks::TaskHookRecordStore,
    service_admission: gents_store::service::RuntimeServiceAdmission,
    #[cfg(test)]
    stop_release_observer: Option<Arc<tokio::sync::Notify>>,
    #[cfg(any(test, feature = "test-fixtures"))]
    release_wait: Option<std::time::Duration>,
}

pub(crate) const LIVE_STOP_RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackgroundShutdownReport {
    pub stopped: usize,
    pub unstopped: usize,
}

impl BackgroundExecutionRegistry {
    /// Bounds a stop's wait for its worker's release by `wait` instead of
    /// `LIVE_STOP_RELEASE_WAIT`: a fixture whose property is not that bound
    /// puts it at its hang ceiling, out of reach of a loaded host.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_release_wait(mut self, wait: std::time::Duration) -> Self {
        self.release_wait = Some(wait);
        self
    }

    fn release_wait(&self) -> std::time::Duration {
        #[cfg(any(test, feature = "test-fixtures"))]
        if let Some(wait) = self.release_wait {
            return wait;
        }
        LIVE_STOP_RELEASE_WAIT
    }

    #[cfg(test)]
    pub(crate) fn with_stop_release_observer(mut self, observer: Arc<tokio::sync::Notify>) -> Self {
        self.stop_release_observer = Some(observer);
        self
    }

    pub fn service_admission(&self) -> &gents_store::service::RuntimeServiceAdmission {
        &self.service_admission
    }

    /// Keep durable host records of spawned background processes in `dir`,
    /// which must belong to this runtime's exclusively locked store; a
    /// restarted runtime otherwise cannot prove it owns a surviving process.
    pub fn with_process_records(mut self, dir: PathBuf) -> Self {
        self.process_records = gents_session::exec::ownership::ProcessRecordStore::Durable(dir);
        self
    }

    pub fn with_task_hook_records(mut self, dir: PathBuf) -> Self {
        self.task_hook_records = crate::call::task_hooks::TaskHookRecordStore::durable(dir);
        self
    }

    pub fn task_hook_records(&self) -> &crate::call::task_hooks::TaskHookRecordStore {
        &self.task_hook_records
    }

    pub(crate) fn process_recorder(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> gents_session::exec::ownership::ProcessRecorder {
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
        let recorder = gents_session::exec::ownership::ProcessRecorder::new(move |identity| {
            if let Some(owner) = &identity_owner {
                *owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(identity.clone());
            }
            let record = gents_session::exec::ownership::ProcessRecord {
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

    pub fn process_record(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> Option<gents_session::exec::ownership::ProcessRecord> {
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
                Some(gents_session::exec::ownership::ProcessRecord {
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
    ) -> gents_session::exec::ownership::LiveProcessEvidence {
        self.lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .map(|execution| execution.process_evidence.get())
            .unwrap_or(gents_session::exec::ownership::LiveProcessEvidence::SpawnedUnverified)
    }

    pub(super) fn live_process_evidence_owner(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> Option<Arc<gents_session::exec::ownership::LiveProcessEvidenceOwner>> {
        self.lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .map(|execution| execution.process_evidence.clone())
    }

    pub(super) async fn process_exit_confirmed(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> bool {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            matches!(
                record
                    .identity
                    .await_signalled_group_exit(gents_session::exec::ownership::KILL_GRACE)
                    .await,
                gents_session::exec::ownership::ProcessObservation::Exited
            )
        } else {
            self.live_process_evidence(tool_call_id, tool_call_doc_id)
                != gents_session::exec::ownership::LiveProcessEvidence::SpawnedUnverified
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

    pub(super) fn set_stop_receipt(&self, call: &str, doc: &str, receipt: RetainedStopReceipt) {
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

    pub(super) fn remember_confirmed_stop(&self, execution: &BackgroundExecution) {
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

    pub(super) fn finalize_pending_failure(
        &self,
        call: &str,
        doc: &str,
    ) -> Option<gents_loop::tool_call_lifecycle::ToolOutcome> {
        if !self.has_pending_failure(call, doc) {
            return None;
        }
        let outcome = gents_loop::tool_call_lifecycle::ToolOutcome::tool_returned_error(
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
        if retire && gents_session::exec::ownership::retire_stop_receipt(terminal_committed) {
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

    pub(crate) fn process_record_list(&self) -> Vec<gents_session::exec::ownership::ProcessRecord> {
        self.process_records.list()
    }

    pub fn try_process_record_list(
        &self,
    ) -> anyhow::Result<Vec<gents_session::exec::ownership::ProcessRecord>> {
        self.process_records.try_list()
    }

    pub(crate) async fn release_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            let observed = record
                .identity
                .await_leader_exit(gents_session::exec::ownership::KILL_GRACE)
                .await;
            if observed.retain_process_record() {
                self.note_retained_process_record(&record, observed);
                return;
            }
            self.remove_process_record(tool_call_id, tool_call_doc_id);
        }
    }

    pub(crate) fn forget_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            let observed = record.identity.observe();
            if observed.retain_process_record() {
                self.note_retained_process_record(&record, observed);
                return;
            }
            self.remove_process_record(tool_call_id, tool_call_doc_id);
        }
    }

    fn note_retained_process_record(
        &self,
        record: &gents_session::exec::ownership::ProcessRecord,
        observed: gents_session::exec::ownership::ProcessObservation,
    ) {
        let first = self
            .unproven_record_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(record.tool_call_doc_id.clone());
        if first {
            tracing::warn!(
                tool_call_id = %record.tool_call_id,
                tool_call_doc_id = %record.tool_call_doc_id,
                pid = record.identity.pid,
                pgid = record.identity.pgid,
                observation = ?observed,
                "background process group is not proven exited; keeping its record"
            );
        } else {
            tracing::debug!(
                tool_call_id = %record.tool_call_id,
                tool_call_doc_id = %record.tool_call_doc_id,
                pid = record.identity.pid,
                pgid = record.identity.pgid,
                observation = ?observed,
                "background process group is still not proven exited; keeping its record"
            );
        }
    }

    fn remove_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        self.unproven_record_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(tool_call_doc_id);
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
    ) -> gents_session::exec::ProcessStopOutcome {
        self.stop_execution_inner(tool_call_id, tool_call_doc_id, false)
            .await
    }

    pub(crate) async fn stop_native_process(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> gents_session::exec::ProcessStopOutcome {
        self.stop_execution_inner(tool_call_id, tool_call_doc_id, true)
            .await
    }

    pub async fn shutdown(&self) -> BackgroundShutdownReport {
        let live = self
            .lock_executions()
            .values()
            .map(|execution| {
                (
                    execution.tool_call_id.clone(),
                    execution.tool_call_doc_id.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut stops = tokio::task::JoinSet::new();
        for (tool_call_id, tool_call_doc_id) in live {
            let registry = self.clone();
            stops.spawn(async move {
                let outcome = registry
                    .stop_native_process(&tool_call_id, &tool_call_doc_id)
                    .await;
                (tool_call_id, tool_call_doc_id, outcome)
            });
        }
        let mut report = BackgroundShutdownReport::default();
        while let Some(joined) = stops.join_next().await {
            match joined {
                Ok((tool_call_id, tool_call_doc_id, outcome)) => {
                    if matches!(
                        outcome,
                        gents_session::exec::ProcessStopOutcome::Stopped
                            | gents_session::exec::ProcessStopOutcome::AlreadyExited
                    ) {
                        report.stopped += 1;
                    } else {
                        report.unstopped += 1;
                        tracing::warn!(
                            %tool_call_id,
                            %tool_call_doc_id,
                            outcome = outcome.as_str(),
                            "background process was not observed to stop during shutdown; \
                             its record is left for recovery"
                        );
                    }
                }
                Err(error) => {
                    report.unstopped += 1;
                    tracing::warn!(%error, "background process stop failed to join during shutdown");
                }
            }
        }
        report
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
    ) -> gents_session::exec::ProcessStopOutcome {
        use gents_session::exec::ownership::ProcessObservation;
        use gents_session::exec::ProcessStopOutcome;
        let record = self.process_record(tool_call_id, tool_call_doc_id);
        let execution = self
            .lock_executions()
            .get(tool_call_doc_id)
            .filter(|execution| execution.tool_call_id == tool_call_id)
            .cloned();
        if let Some(execution) = execution {
            let completion_rx = execution.worker_release_tx.subscribe();
            execution.cancellation_token.cancel();
            let released =
                tokio::time::timeout(self.release_wait(), Self::await_completion(completion_rx))
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
                gents_session::exec::ownership::LiveProcessEvidence::SpawnedUnverified
            } else if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
                match record
                    .identity
                    .await_signalled_group_exit(gents_session::exec::ownership::KILL_GRACE)
                    .await
                {
                    ProcessObservation::Exited => {
                        gents_session::exec::ownership::LiveProcessEvidence::Exited
                    }
                    _ => gents_session::exec::ownership::LiveProcessEvidence::SpawnedUnverified,
                }
            } else if require_record {
                execution.process_evidence.get()
            } else {
                gents_session::exec::ownership::LiveProcessEvidence::Exited
            };
            let outcome = gents_session::exec::ownership::settle_live_cancel(released, evidence);
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

    /// Reads a retained process output window as `access`'s actor, which
    /// must own the tool call: rows are Agent documents under ACP.
    pub async fn read_process_output_snapshot(
        &self,
        access: &gents_store::config_client::ConfigAccess,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        tool_call_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let caller = crate::background::status::ProcessControlScope {
            request_id: String::new(),
            session_id: session_id.to_owned(),
            node_did: node_did.to_owned(),
            requester_did: requester_did.map(str::to_owned),
        };
        let result = crate::background::status::read_tool_output_slice_with_access(
            access,
            &caller,
            &self.live_outputs.registry,
            tool_call_id,
            0,
            usize::MAX,
        )
        .await?;
        Ok(match result {
            crate::background::status::ReadToolOutputOutcome::Found(page) => {
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

    pub fn active_tool_call_doc_ids(&self) -> Vec<String> {
        self.lock_executions()
            .values()
            .map(|execution| execution.tool_call_doc_id.clone())
            .collect()
    }

    pub(super) fn retain_confirmed_worker(
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

    pub(super) async fn retain_terminal_settlement(
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
                gents_loop::tool_call_lifecycle::ToolOutcome::Failed {
                    denial: Some(denial),
                    ..
                } => {
                    lifecycle
                        .fail_with_command_denial(outcome.model_facing_text(), denial)
                        .await?
                }
                gents_loop::tool_call_lifecycle::ToolOutcome::Failed { class, .. } => {
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
                let fresh = ToolCallLifecycle::load_by_doc_id_as(
                    lifecycle.node().clone(),
                    lifecycle.actor().clone(),
                    &doc,
                    lifecycle.node_did(),
                    lifecycle.session_id(),
                    lifecycle.requester_did(),
                )
                .await?;
                match fresh {
                    Some(mut fresh) if fresh.is_terminal() => {
                        fresh.inherit_signer(lifecycle)?;
                        *lifecycle = fresh;
                        false
                    }
                    _ => return Err(error),
                }
            }
        };
        if lifecycle.is_terminal() {
            if won && lifecycle.await_mode() == gents_protocol::tool_call::AwaitMode::Background {
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
                            let raw = gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
                                &lifecycle.access()?,
                                &doc,
                                request_doc,
                                lifecycle.session_id(),
                                lifecycle.node_did(),
                                lifecycle.requester_did(),
                            )
                            .await?
                            .into_text();
                            ("failed", raw, Some("tool_failed"))
                        }
                    };
                    crate::background::completion::append_background_tool_completion(lifecycle.node(), &lifecycle.access()?, &crate::background::completion::BackgroundToolCompletion { parent_session_id: lifecycle.session_id(), parent_request_id: &calling_request_id, tool_call_doc_id: &doc, tool_name: lifecycle.tool_name(), status, result: &output, reason, wake: gents_protocol::request_lifecycle::RequestHopCause::Continuation })
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
                            == gents_session::exec::ownership::ProcessObservation::Exited
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

    pub(super) fn lock_executions(
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
    _service_work: Arc<gents_store::service::RuntimeServiceWork>,
}

impl BackgroundExecutionReservation {
    pub(super) fn retain_for_recovery(self, lifecycle: ToolCallLifecycle) {
        self.retain_stopped_worker(lifecycle, true);
    }

    pub(super) fn retain_stopped_worker(
        mut self,
        lifecycle: ToolCallLifecycle,
        handoff_pending: bool,
    ) {
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
