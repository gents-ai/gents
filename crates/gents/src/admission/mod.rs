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
    current_call_join, current_session_id, scope_call, scope_call_with_join,
    scope_call_with_token_and_failure_reason, scope_request, set_terminal_failure_reason,
    terminal_failure_reason_observer, AdmissionCallContext, AdmittedCompletionClient, CallKind,
};
pub(crate) use config::backend_admission_configs_from_backends;
pub(crate) use config::exportable_backend_fingerprint;
pub(crate) use config::BackendAvailability;
pub use config::{document_configured_from_fields, BackendAdmissionConfig};
pub use recovery::{InferenceCall, InferenceCallRecoveryReport};
pub(crate) use registry::AdmissionRegistry;
pub use slot_accounting::call_state_holds_backend_slot;

#[cfg(test)]
mod tests;
