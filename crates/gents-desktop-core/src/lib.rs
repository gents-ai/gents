//! Client adapter layer: reusable desktop client runtime, preferences, and home reset helpers.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Clients/`.

pub mod ansi;
pub mod client;
pub mod home_reset;
pub mod preference;

#[cfg(test)]
mod access_classification_tests;
