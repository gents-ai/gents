mod client;
mod config;
mod controller;
mod permit;
mod persistence;
mod registry;
#[allow(dead_code)]
mod slot_accounting;
pub(crate) mod stream_guard;

pub(crate) use client::{
    scope_call, scope_call_with_token, scope_request, AdmissionCallContext,
    AdmittedCompletionClient, CallKind,
};
pub(crate) use config::{backend_admission_configs_from_backends, BackendAdmissionConfig};
pub(crate) use registry::AdmissionRegistry;

#[cfg(test)]
mod tests;
