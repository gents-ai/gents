use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigExecutionReceipt {
    pub version: u8,
    /// Deliberately does not assert whether that operation committed.
    pub mutation_entered: bool,
}

#[derive(Default)]
pub(super) struct ExecutionObservation(AtomicBool);

impl ExecutionObservation {
    pub(super) fn enter_mutation(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(super) fn receipt(&self) -> ConfigExecutionReceipt {
        ConfigExecutionReceipt {
            version: 1,
            mutation_entered: self.0.load(Ordering::Relaxed),
        }
    }
}
