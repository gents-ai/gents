use std::collections::BTreeSet;

pub use gents::agent::p2p_reconcile::{PairingApplied, PairingDesired};

pub mod invariants;
pub mod runner;
pub mod scenario;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingActual {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    pub connected: bool,
}
