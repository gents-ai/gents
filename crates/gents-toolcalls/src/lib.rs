//! Tool calls layer: tool-call dispatch, lifecycle, background jobs, and plugin host calls.
//!
//! Lean models live in `crates/gents/proofs/Proofs/ToolCalls/`; test fixtures live in `src/test_support/` (`test-fixtures`).

#[cfg(test)]
mod backend_observation_read_tests;
pub mod background;
pub mod call;
#[cfg(test)]
mod inference_parent_recovery_tests;
pub mod plugin;
#[cfg(test)]
mod rendered_request_sink_tests;
#[cfg(test)]
mod self_config_tests;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod views;

#[cfg(test)]
mod access_classification_tests;
