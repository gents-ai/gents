mod client;
mod config;
mod controller;
mod permit;
mod persistence;
mod recovery;
mod registry;
mod slot_accounting;
pub(crate) mod stream_guard;

pub(crate) use client::{
    current_call_join, current_session_id, scope_backend, AdmissionError, AdmittedCallError,
};
pub use client::{
    scope_call, scope_call_with_join, scope_call_with_token_and_failure_reason, scope_request,
    set_terminal_failure_reason, terminal_failure_reason_observer,
};
pub use client::{AdmissionCallContext, AdmittedCompletionModel, CallKind};
pub(crate) use config::backend_connection_fingerprint;
pub use config::exportable_backend_fingerprint;
pub use config::BackendAvailability;
pub use config::{document_configured_from_fields, BackendAdmissionConfig};
pub(crate) use persistence::ProviderCallUsage;
pub use recovery::{InferenceCall, InferenceCallRecoveryReport};
pub use registry::AdmissionRegistry;
pub use slot_accounting::call_state_holds_backend_slot;

/// The recovery and persistence owners' items that full-request tests above
/// this crate drive, because a request exists only in higher layers.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod recovery_fixtures {
    pub use super::controller::{InferenceCallRecord, PendingCallMetadata};
    pub use super::persistence::{persist_existing_call_terminal, ProviderCallUsage};
    pub use super::recovery::{
        load_stale_inference_calls, lookup_parent_request, recover_inference_call_row,
        recovery_outcome, InferenceRecoveryOutcome, StaleInferenceCallRow,
    };
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod slot_accounting_tests;
