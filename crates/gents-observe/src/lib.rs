//! Observe layer: run timelines, trace export, projections, and file inventory.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Observe/`; test fixtures live in `src/test_support/` (`test-fixtures`).

pub mod client_file_review;
pub mod client_inference;
pub mod client_projection;
pub mod file_inventory;
pub mod observe;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
