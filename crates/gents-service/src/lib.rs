//! Host layer: home state, native service supervision, runtime control, and managed updates.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Clients/`; test fixtures live in `src/test_support.rs` (`test-fixtures`).

pub mod args;
pub mod enrollment_admin;
pub mod home_state;
pub mod lifecycle;
pub mod native_service;
pub mod network_settings;
pub mod node_port;
pub mod process_resources;
pub mod repair;
pub mod runtime_control;
pub mod server_host;
pub mod service_update;
pub mod state;
pub mod telemetry;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;

#[cfg(test)]
mod access_classification_tests;
