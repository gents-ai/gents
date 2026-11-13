use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageFaultRecord {
    pub observed_at: DateTime<Utc>,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct StorageFaultObservation(Arc<RwLock<Option<StorageFaultRecord>>>);

impl StorageFaultObservation {
    pub fn new() -> Self {
        Self::default()
    }

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

    pub fn observation(&self) -> gents_protocol::node_readiness::RuntimeStorageObservation {
        gents_protocol::node_readiness::RuntimeStorageObservation::from_unavailable(
            self.is_unavailable(),
        )
    }
}

pub(crate) fn is_unwritable_store(error: &anyhow::Error) -> bool {
    gents_store::graphql::is_storage_full(error.as_ref())
}

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
