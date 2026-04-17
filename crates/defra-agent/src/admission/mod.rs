pub(crate) mod stream_guard;
mod config;
mod client;
mod controller;
mod permit;
mod registry;
mod persistence;

pub(crate) use config::{backend_admission_configs_from_backends, BackendAdmissionConfig};
pub(crate) use client::{
    scope_call, scope_request, AdmissionCallContext, AdmittedCompletionClient, CallKind,
};
pub(crate) use registry::AdmissionRegistry;

#[cfg(test)]
mod tests;
