//! Low-cardinality telemetry for the committed-write owner: queries,
//! endpoints, identities, document IDs, and errors never enter these types.

use std::time::Duration;

pub(crate) const WRITE_ATTEMPT_EVENT_TARGET: &str = "gents.defradb.write_attempt";

pub(super) fn record_slow_native_phase(
    operation: Option<&'static str>,
    phase: &'static str,
    elapsed: Duration,
) {
    if elapsed >= Duration::from_secs(1) {
        tracing::warn!(
            target: "gents.defradb.transaction_phase",
            operation = operation.unwrap_or("unlabeled"),
            phase,
            elapsed_ms = millis(elapsed),
            "slow native transaction phase"
        );
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteTelemetryError {
    #[error("invalid static write operation name")]
    InvalidOperationName,
    #[error("invalid write attempt")]
    InvalidAttempt,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteOperation(&'static str);

impl WriteOperation {
    pub(crate) fn new(name: &'static str) -> Result<Self, WriteTelemetryError> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.')
            });
        if !valid {
            return Err(WriteTelemetryError::InvalidOperationName);
        }
        Ok(Self(name))
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum WriteBackend {
    Embedded,
    Http,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum WriteMode {
    AutoCommit,
    Transaction,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RetryOwner {
    DefraDb,
    GentsIdempotentTransaction,
    GentsTransaction,
    GentsReceipt,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum WriteOutcome {
    Committed,
    ConflictObserved,
    Recovered,
    Retrying,
    PendingNative,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ReceiptRecovery {
    NotAttempted,
    StableIdConfirmed,
    StableIdAbsent,
    StableIdReadFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConflictSource {
    None,
    StructuredCode,
    TypedError,
    HttpStatusTextFallback,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RollbackStatus {
    NotNeeded,
    Succeeded,
    Scheduled,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteAttemptOrdinal {
    attempt: u32,
    max_attempts: u32,
}

impl WriteAttemptOrdinal {
    pub(crate) fn new(attempt: u32, max_attempts: u32) -> Result<Self, WriteTelemetryError> {
        if attempt == 0 || attempt > max_attempts {
            return Err(WriteTelemetryError::InvalidAttempt);
        }
        Ok(Self {
            attempt,
            max_attempts,
        })
    }
}

pub(crate) struct WriteAttemptEvent {
    pub(crate) operation: WriteOperation,
    pub(crate) backend: WriteBackend,
    pub(crate) mode: WriteMode,
    pub(crate) retry_owner: RetryOwner,
    pub(crate) ordinal: WriteAttemptOrdinal,
    pub(crate) outcome: WriteOutcome,
    pub(crate) conflict_source: ConflictSource,
    pub(crate) receipt_recovery: ReceiptRecovery,
    pub(crate) backoff: Option<Duration>,
    pub(crate) elapsed: Duration,
    pub(crate) affected_documents: Option<u64>,
    pub(crate) rollback: RollbackStatus,
}

impl WriteAttemptEvent {
    pub(crate) fn record(self) {
        let attempt = self.ordinal.attempt;
        let max_attempts = self.ordinal.max_attempts;
        let retry_index = attempt - 1;
        let max_retries = max_attempts - 1;
        macro_rules! emit {
            ($level:expr) => {
                tracing::event!(
            target: WRITE_ATTEMPT_EVENT_TARGET,
            $level,
            operation = self.operation.0,
            backend = match self.backend { WriteBackend::Embedded => "embedded", WriteBackend::Http => "http" },
            mode = match self.mode { WriteMode::AutoCommit => "auto_commit", WriteMode::Transaction => "transaction" },
            retry_owner = match self.retry_owner {
                RetryOwner::DefraDb => "defradb",
                RetryOwner::GentsIdempotentTransaction => "gents_idempotent_transaction",
                RetryOwner::GentsTransaction => "gents_transaction",
                RetryOwner::GentsReceipt => "gents_receipt",
            },
            attempt,
            retry_index,
            max_attempts,
            max_retries,
            retry_count_known = !matches!(self.retry_owner, RetryOwner::DefraDb),
            outcome = match self.outcome {
                WriteOutcome::Committed => "committed",
                WriteOutcome::ConflictObserved => "conflict_observed",
                WriteOutcome::Recovered => "recovered",
                WriteOutcome::Retrying => "retrying",
                WriteOutcome::PendingNative => "pending_native",
                WriteOutcome::Failed => "failed",
                WriteOutcome::Cancelled => "cancelled",
            },
            conflict = if self.conflict_source == ConflictSource::None { "none" } else { "transaction" },
            conflict_source = match self.conflict_source {
                ConflictSource::None => "none",
                ConflictSource::StructuredCode => "structured_code",
                ConflictSource::TypedError => "typed_error",
                ConflictSource::HttpStatusTextFallback => "http_status_text_fallback",
            },
            receipt_recovery = match self.receipt_recovery {
                ReceiptRecovery::NotAttempted => "not_attempted",
                ReceiptRecovery::StableIdConfirmed => "stable_id_confirmed",
                ReceiptRecovery::StableIdAbsent => "stable_id_absent",
                ReceiptRecovery::StableIdReadFailed => "stable_id_read_failed",
            },
            receipt_recovered = matches!(self.outcome, WriteOutcome::Recovered),
            conflict_observed = matches!(self.outcome, WriteOutcome::ConflictObserved),
            will_retry = matches!(self.outcome, WriteOutcome::Retrying),
            settled = !matches!(self.outcome, WriteOutcome::PendingNative) && !matches!(self.rollback, RollbackStatus::Scheduled),
            backoff_scheduled = self.backoff.is_some(),
            backoff_ms = millis(self.backoff.unwrap_or_default()),
            elapsed_ms = millis(self.elapsed),
            affected_documents_known = self.affected_documents.is_some(),
            affected_documents = self.affected_documents.unwrap_or_default(),
            rollback = match self.rollback {
                RollbackStatus::NotNeeded => "not_needed",
                RollbackStatus::Succeeded => "succeeded",
                RollbackStatus::Scheduled => "scheduled",
            },
            cancelled = matches!(self.outcome, WriteOutcome::Cancelled),
            "DefraDB write attempt observation"
                );
            };
        }
        if self.is_clean_committed_noop() {
            emit!(tracing::Level::DEBUG);
        } else {
            emit!(tracing::Level::INFO);
        }
    }

    fn is_clean_committed_noop(&self) -> bool {
        matches!(self.outcome, WriteOutcome::Committed)
            && !matches!(self.retry_owner, RetryOwner::DefraDb)
            && self.ordinal.attempt == 1
            && self.conflict_source == ConflictSource::None
            && matches!(self.receipt_recovery, ReceiptRecovery::NotAttempted)
            && self.backoff.is_none()
            && self.affected_documents == Some(0)
            && matches!(self.rollback, RollbackStatus::NotNeeded)
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
