use std::collections::BTreeSet;

pub mod invariants;
pub mod runner;
pub mod scenario;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingDesired {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

impl PairingDesired {
    pub fn has_wiring(&self) -> bool {
        !self.collections.is_empty() || !self.replicator_addresses.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingActual {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    pub connected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingApplied {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}
