//! Tools layer: builtin, native, plugin, and MCP tool surfaces plus self-configuration grants.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Tools/`; test fixtures live in `src/test_support/` (`test-fixtures`).

pub mod builtin;
mod collection_use;
mod guided_failure;
pub mod mcp;
pub mod memory;
pub mod native;
pub mod plugin;
pub mod plugin_host;
pub mod rendering;
pub mod runtime_context;
pub mod skill_pointer;
pub mod surface;
pub mod task_run;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod tools_rebinding;
pub mod ux_data;
pub mod ux_preview;

#[cfg(test)]
mod documents_skill_tests;

#[cfg(test)]
mod documents_selection_tests;

#[cfg(test)]
mod access_classification_tests;
