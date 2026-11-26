//! Client adapter layer: evaluation authoring, checks, runners, and optimization drivers.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Runtime/`; unit test doubles live in `src/test_support/`.

pub mod authoring;
pub mod checks;
pub mod optimization;
pub mod report;
pub mod runner;

pub(crate) use gents::eval::*;

#[cfg(test)]
use gents::support;
#[cfg(test)]
mod document_tests;
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod access_classification_tests;
