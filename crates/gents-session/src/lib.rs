//! Session layer: requests, sessions, execution leases, workspaces, and the lifecycle hook seam.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Session/`; test fixtures live in `src/test_support/` (`test-fixtures`).

pub mod exec;
pub mod file_contents;
pub mod log_rate;
pub mod operations;
pub mod request;
pub mod session;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod tool_call;
pub mod views;
pub mod workspace;

#[cfg(test)]
mod documents_task_selection_tests;

#[cfg(test)]
mod access_classification_tests;
