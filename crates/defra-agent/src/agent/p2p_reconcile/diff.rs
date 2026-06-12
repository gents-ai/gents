//! Pure desired-vs-actual diff for pairing reconcile.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Operator-set desired pairing for one peer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingDesired {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

/// Actual pairing state read from the remote.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingActual {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

/// One emit-an-RPC instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    InstallCollection(String),
    TeardownCollection(String),
    InstallReplicator(String),
    TeardownReplicator(String),
}

/// Diff in canonical sorted order: collections first, then replicators.
pub fn compute_pairing_diff(desired: &PairingDesired, actual: &PairingActual) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for c in desired.collections.difference(&actual.collections) {
        ops.push(DiffOp::InstallCollection(c.clone()));
    }
    for c in actual.collections.difference(&desired.collections) {
        ops.push(DiffOp::TeardownCollection(c.clone()));
    }
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .difference(&desired.replicator_addresses)
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn empty_inputs_yield_no_ops() {
        let ops = compute_pairing_diff(&PairingDesired::default(), &PairingActual::default());
        assert!(ops.is_empty());
    }

    #[test]
    fn missing_collection_yields_install() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            ..Default::default()
        };
        let actual = PairingActual::default();
        assert_eq!(
            compute_pairing_diff(&desired, &actual),
            vec![DiffOp::InstallCollection("c1".into())]
        );
    }

    #[test]
    fn extra_collection_yields_teardown() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["c1"]),
            ..Default::default()
        };
        assert_eq!(
            compute_pairing_diff(&desired, &actual),
            vec![DiffOp::TeardownCollection("c1".into())]
        );
    }

    #[test]
    fn same_state_yields_no_ops() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
        };
        let actual = PairingActual {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
        };
        assert!(compute_pairing_diff(&desired, &actual).is_empty());
    }

    #[test]
    fn collections_diff_emits_before_replicators_diff() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
        };
        let actual = PairingActual::default();
        let ops = compute_pairing_diff(&desired, &actual);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], DiffOp::InstallCollection(_)));
        assert!(matches!(ops[1], DiffOp::InstallReplicator(_)));
    }
}
