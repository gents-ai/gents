//! P2P layer: peer directory, node access, enrollment, and sync reconciliation.
//!
//! Lean models live in `crates/gents/proofs/Proofs/P2P/`; test fixtures live in `src/test_support/` (`test-fixtures`).

pub mod directory_projection;
pub mod node_access;
pub mod operations;
pub mod p2p;
pub mod p2p_reconcile;
mod peer_publication;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;

pub mod views;

#[cfg(test)]
mod access_classification_tests;
