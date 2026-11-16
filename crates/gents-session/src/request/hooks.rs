//! Lifecycle data hooks: the seam through which a bundled plugin takes part in
//! a request or session transition without holding its transaction. Lean
//! `Proofs.Automation.Hooks` is the contract; each item names the definition it executes.

pub mod claim;
#[cfg(test)]
mod decision_conformance;
pub mod delivery;
pub mod effect;
pub mod hook;
pub(crate) mod native;
pub mod operations;
pub mod owed;
pub mod points;
pub mod prepared;
#[cfg(test)]
mod pushdown_tests;
pub mod queue_scope;
pub mod registry;
#[cfg(test)]
mod rerun_conformance;
mod revisions;
pub mod stage;
pub mod supersede;
