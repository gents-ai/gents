//! Low-cardinality telemetry for the committed-write owner.
//!
//! Events contain a static operation name, bounded states, counts, and
//! durations. Queries, endpoints, identities, document IDs, and errors never
//! enter this type.

use std::time::Duration;

pub(crate) const WRITE_ATTEMPT_EVENT_TARGET: &str = "gents.defradb.write_attempt";

#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteOperation(&'static str);

impl WriteOperation {
    pub(crate) fn new(name: &'static str) -> anyhow::Result<Self> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.')
            });
        anyhow::ensure!(valid, "invalid static write operation name");
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
    pub(crate) fn new(attempt: u32, max_attempts: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            attempt > 0 && attempt <= max_attempts,
            "invalid write attempt"
        );
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
        tracing::event!(
            target: WRITE_ATTEMPT_EVENT_TARGET,
            tracing::Level::INFO,
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
            "DefraDB committed-write attempt finished"
        );
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::WriteOperation;

    #[test]
    fn operation_names_are_static_and_low_cardinality() {
        assert!(WriteOperation::new("goal.resume_request").is_ok());
        for invalid in ["", "Goal Resume", "did:key:secret", "request/id"] {
            assert!(
                WriteOperation::new(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }
}
