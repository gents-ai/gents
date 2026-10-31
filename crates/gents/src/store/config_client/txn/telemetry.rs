use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use super::{ConfigAccess, ConfigClientError, Result};
use crate::store::config_client::write_telemetry::{
    ConflictSource, ReceiptRecovery, RetryOwner, RollbackStatus, WriteAttemptEvent,
    WriteAttemptOrdinal, WriteBackend, WriteMode, WriteOperation, WriteOutcome,
};

pub(super) fn backend_for_access(access: &ConfigAccess) -> WriteBackend {
    match access {
        ConfigAccess::Graphql(_) => WriteBackend::Http,
        ConfigAccess::Local(_) => WriteBackend::Embedded,
    }
}

pub(super) fn auto_commit_telemetry(
    operation: WriteOperation,
    backend: WriteBackend,
) -> Result<AttemptTelemetry> {
    Ok(AttemptTelemetry {
        operation,
        backend,
        mode: WriteMode::AutoCommit,
        retry_owner: RetryOwner::DefraDb,
        ordinal: WriteAttemptOrdinal::new(1, 1)?,
        started: Instant::now(),
        state: StdMutex::new(AttemptTelemetryState::default()),
        cancelled_rollback: RollbackStatus::NotNeeded,
        cancellation_rollback_scheduled: None,
        dispatch: tracing::dispatcher::get_default(Clone::clone),
    })
}

pub(super) fn finish_auto_commit<T>(
    telemetry: &AttemptTelemetry,
    result: Result<T>,
    affected_documents: impl FnOnce(&T) -> u64,
) -> Result<T> {
    let pending_native = result
        .as_ref()
        .err()
        .is_some_and(ConfigClientError::native_operation_abandoned);
    let affected_documents = if pending_native {
        None
    } else {
        result.as_ref().ok().map(affected_documents).or(Some(0))
    };
    telemetry.record(AttemptReport {
        outcome: if pending_native {
            WriteOutcome::PendingNative
        } else if result.is_ok() {
            WriteOutcome::Committed
        } else {
            WriteOutcome::Failed
        },
        conflict_source: ConflictSource::None,
        backoff: None,
        affected_documents,
        rollback: RollbackStatus::NotNeeded,
    });
    result
}

pub(super) struct AttemptReport {
    pub(super) outcome: WriteOutcome,
    pub(super) conflict_source: ConflictSource,
    pub(super) backoff: Option<std::time::Duration>,
    pub(super) affected_documents: Option<u64>,
    pub(super) rollback: RollbackStatus,
}

#[derive(Default)]
pub(super) struct AttemptTelemetryState {
    finished: bool,
    pending_native: bool,
}

pub(super) struct AttemptTelemetry {
    pub(super) operation: WriteOperation,
    pub(super) backend: WriteBackend,
    pub(super) mode: WriteMode,
    pub(super) retry_owner: RetryOwner,
    pub(super) ordinal: WriteAttemptOrdinal,
    pub(super) started: Instant,
    pub(super) state: StdMutex<AttemptTelemetryState>,
    pub(super) cancelled_rollback: RollbackStatus,
    pub(super) cancellation_rollback_scheduled: Option<Arc<AtomicBool>>,
    pub(super) dispatch: tracing::Dispatch,
}

pub(super) struct AutoCommitCallerObservation {
    pub(super) telemetry: Arc<AttemptTelemetry>,
    pub(super) completed: bool,
}

pub(super) struct AutoCommitNativeCompletion {
    pub(super) telemetry: Arc<AttemptTelemetry>,
    pub(super) started: bool,
    pub(super) completed: bool,
}

impl AutoCommitCallerObservation {
    pub(super) fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for AutoCommitCallerObservation {
    fn drop(&mut self) {
        if !self.completed {
            self.telemetry.record_pending_native();
        }
    }
}

impl Drop for AutoCommitNativeCompletion {
    fn drop(&mut self) {
        if self.started && !self.completed {
            self.telemetry.record_pending_native();
        }
    }
}

impl AttemptTelemetry {
    pub(super) fn record(&self, report: AttemptReport) {
        self.record_with_receipt(report, ReceiptRecovery::NotAttempted);
    }

    pub(super) fn record_with_receipt(
        &self,
        report: AttemptReport,
        receipt_recovery: ReceiptRecovery,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finished {
            return;
        }
        state.finished = true;
        let event = WriteAttemptEvent {
            operation: self.operation,
            backend: self.backend,
            mode: self.mode,
            retry_owner: self.retry_owner,
            ordinal: self.ordinal,
            outcome: report.outcome,
            conflict_source: report.conflict_source,
            receipt_recovery,
            backoff: report.backoff,
            elapsed: self.started.elapsed(),
            affected_documents: report.affected_documents,
            rollback: report.rollback,
        };
        tracing::dispatcher::with_default(&self.dispatch, || {
            event.record();
        });
    }

    fn record_pending_native(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finished || state.pending_native {
            return;
        }
        state.pending_native = true;
        let event = WriteAttemptEvent {
            operation: self.operation,
            backend: self.backend,
            mode: self.mode,
            retry_owner: self.retry_owner,
            ordinal: self.ordinal,
            outcome: WriteOutcome::PendingNative,
            conflict_source: ConflictSource::None,
            receipt_recovery: ReceiptRecovery::NotAttempted,
            backoff: None,
            elapsed: self.started.elapsed(),
            affected_documents: None,
            rollback: RollbackStatus::NotNeeded,
        };
        tracing::dispatcher::with_default(&self.dispatch, || event.record());
    }
}

impl Drop for AttemptTelemetry {
    fn drop(&mut self) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finished || state.pending_native {
            return;
        }
        drop(state);
        let rollback = if self
            .cancellation_rollback_scheduled
            .as_ref()
            .is_some_and(|scheduled| scheduled.load(Ordering::Acquire))
        {
            RollbackStatus::Scheduled
        } else {
            self.cancelled_rollback
        };
        self.record(AttemptReport {
            outcome: WriteOutcome::Cancelled,
            conflict_source: ConflictSource::None,
            backoff: None,
            affected_documents: if matches!(rollback, RollbackStatus::Scheduled) {
                None
            } else {
                Some(0)
            },
            rollback,
        });
    }
}
