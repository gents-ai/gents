mod apply_txn;
mod auto_commit;
mod cursor;
mod execute;
#[cfg(test)]
mod fault_injection;
mod finalize;
mod gate;
mod native;
mod rollback;
mod telemetry;
mod transact;
mod transact_entry;

use std::time::Duration;

use super::graphql;
use super::retry;
use super::{
    graphql_api_base, ConfigAccess, ConfigClientError, GraphqlEndpoint, TransactionFailure,
};

pub use apply_txn::ConfigApplyTxn;
pub use gate::LocalConfigAccess;
pub use native::{NativeOperationAbandoned, ReentrantEmbeddedWrite};
pub use transact::IdempotentTransactionRetry;

/// Tracing target of DefraDB transaction and read telemetry; tests observe it by this name.
pub const TRACING_TARGET: &str = module_path!();

#[cfg(test)]
use super::write_telemetry::WriteOperation;
#[cfg(test)]
use apply_txn::{EmbeddedTransactionPurpose, TxnBackend};
#[cfg(test)]
use fault_injection::{EmbeddedFinalizationBoundary, EMBEDDED_FINALIZATION_PAUSE};
#[cfg(test)]
use gate::{
    existing_mutation_write_gate, mutation_write_gate, MutationWriteGate, MutationWriteGuard,
    MUTATION_WRITE_GATE_CAPACITY,
};
#[cfg(test)]
use native::NativeOperationTracker;
#[cfg(test)]
use rollback::RollbackDropCleanupPause;
#[cfg(test)]
use rollback::{begin_embedded_owned, cleanup_while_holding_write_gate};
#[cfg(test)]
use transact::{transact_owned, TransactionMode, TransactionOutcome};

type Result<T, E = ConfigClientError> = std::result::Result<T, E>;

/// Failure of a stable-id receipt write: the write itself, or the caller's
/// receipt read after an ambiguous attempt.
/// The receipt read is the caller's own query, so its failure stays opaque.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptWriteError {
    #[error(transparent)]
    Write(#[from] ConfigClientError),
    #[error("checking stable write receipt")]
    Receipt(#[source] Box<dyn std::error::Error + Send + Sync>),
}

// Bound every phase retaining the process-wide mutation gate so one wedged
// transaction cannot stop response progress, recovery, and hydration.
pub(crate) const EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT: Duration = Duration::from_secs(60);
const EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT: Duration = Duration::from_secs(15);

fn embedded_phase_timeout(phase: &'static str, timeout: Duration) -> ConfigClientError {
    tracing::error!(
        phase,
        timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        "embedded DefraDB canonical write phase timed out"
    );
    retry::transaction_storage_failure(ConfigClientError::PhaseTimeout { phase, timeout })
}

#[cfg(test)]
#[path = "txn_owner_tests.rs"]
mod owner_tests;

#[cfg(test)]
#[path = "txn_native_completion_conformance.rs"]
mod native_completion_conformance;

#[cfg(test)]
#[path = "txn_native_claim_lifetime_conformance.rs"]
mod native_claim_lifetime_conformance;

#[cfg(test)]
#[path = "receipt_owner_tests.rs"]
mod receipt_owner_tests;

#[cfg(test)]
#[path = "txn_stream_waiter_conformance.rs"]
mod stream_waiter_conformance;

#[cfg(test)]
mod variable_tests;
