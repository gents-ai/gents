//! Packs layer: capability packs, plugin grants, and the WASM plugin runner.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Pack/`; test fixtures live in `src/test_support.rs` (`test-fixtures`).

pub mod operations;
pub mod pack;
pub mod plugin;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod views;

#[cfg(test)]
mod documents_references_tests;
