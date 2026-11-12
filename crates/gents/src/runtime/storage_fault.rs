//! Process-local storage-fault observation.

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};

/// What a reader needs to explain the fault and act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageFaultRecord {
    pub observed_at: DateTime<Utc>,
    pub detail: String,
}

/// The runtime's own answer to "can this process still write?".
///
/// A write-ahead log append that fails cannot be rolled out of the log, so the
/// storage engine latches the failure for the life of the process: every later
/// write fails with the retained error kind, and only a new engine over the
/// same directory clears it. The durable readiness row is one of those writes,
/// so a latched runtime cannot record "I am not writable" anywhere a reader
/// would look; it would keep publishing the last row it managed to write.
///
/// This observation is therefore deliberately non-durable and process-local:
/// it lives only as long as the process that cannot write, and readers join it
/// with the frozen row (`RuntimeStorageObservation`). It is lost on exit, which
/// is correct — the recovery it calls for is a restart of this process.
#[derive(Clone, Debug, Default)]
pub struct StorageFaultObservation(Arc<RwLock<Option<StorageFaultRecord>>>);

impl StorageFaultObservation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that this process's store refused a write. The first fault is
    /// retained: it is the one that latched the engine, and later failures are
    /// consequences of it.
    pub fn record_unavailable(&self, detail: impl Into<String>) {
        let mut current = match self.0.write() {
            Ok(current) => current,
            Err(poisoned) => poisoned.into_inner(),
        };
        if current.is_some() {
            return;
        }
        *current = Some(StorageFaultRecord {
            observed_at: Utc::now(),
            detail: detail.into(),
        });
    }

    pub fn current(&self) -> Option<StorageFaultRecord> {
        match self.0.read() {
            Ok(current) => current.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn is_unavailable(&self) -> bool {
        match self.0.read() {
            Ok(current) => current.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        }
    }

    pub fn observation(&self) -> gents_protocol::row::RuntimeStorageObservation {
        gents_protocol::row::RuntimeStorageObservation::from_unavailable(self.is_unavailable())
    }
}

/// Whether this failure is the store refusing writes for the rest of the
/// process's life, rather than a fault of the step that hit it.
pub(crate) fn is_unwritable_store(error: &anyhow::Error) -> bool {
    crate::store::graphql::is_storage_full(error.as_ref())
}

/// Ordered teardown steps record their state durably, which a latched store
/// refuses. The process still stops, the home is still released, and the
/// reason is already retained in `StorageFaultObservation`, so the step
/// continues instead of turning the incident into a teardown failure.
/// Becoming ready is deliberately not tolerant this way: admission must not
/// open on a state no reader can see.
pub(crate) fn tolerate_unwritable_store(persisted: anyhow::Result<()>) -> anyhow::Result<()> {
    match persisted {
        Err(error) if is_unwritable_store(&error) => {
            tracing::warn!(
                error = %error,
                "runtime storage is unavailable; the shutdown state was not persisted"
            );
            Ok(())
        }
        other => other,
    }
}
