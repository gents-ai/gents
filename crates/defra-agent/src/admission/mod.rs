mod client;
mod config;
mod controller;
mod permit;
mod persistence;
mod recovery;
mod registry;
#[cfg(test)]
mod slot_accounting;
pub(crate) mod stream_guard;

pub(crate) use client::{
    scope_call, scope_call_with_token, scope_request, AdmissionCallContext,
    AdmittedCompletionClient, CallKind,
};
pub(crate) use config::backend_admission_configs_from_backends;
pub use config::BackendAdmissionConfig;
pub use recovery::{InferenceCall, InferenceCallRecoveryReport};
pub(crate) use registry::AdmissionRegistry;

#[cfg(test)]
mod tests;
