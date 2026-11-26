//! Store layer: DefraDB access, transactions, identity, custody, and ACP authorization.
//!
//! Lean models live in `crates/gents/proofs/Proofs/Store/`; test fixtures live in `src/test_support/` (`test-fixtures`).

pub mod agent_identity_watch;
pub mod client_error;
pub mod client_journal;
pub mod collection;
pub mod config_client;
pub mod drop_cleanup;
pub mod graphql;
pub mod identity;
pub mod local;
pub mod node_extension;
pub mod provenance;
pub mod schema;
pub mod service;
pub mod subscription_source;
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub mod test_support;
pub mod views;
