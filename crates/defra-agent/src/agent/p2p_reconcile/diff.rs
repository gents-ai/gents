//! Pure desired-vs-actual diff for pairing reconcile.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Operator-set desired pairing for one peer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingDesired {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

impl PairingDesired {
    pub fn has_wiring(&self) -> bool {
        !self.collections.is_empty() || !self.replicator_addresses.is_empty()
    }
}

/// Actual pairing state read from the remote.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingActual {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

/// Reconciler-owned pairing state persisted after successful operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingApplied {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

impl PairingApplied {
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty() && self.replicator_addresses.is_empty()
    }
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

/// Ownership-safe diff: install desired gaps, but only tear down actual extras
/// that this reconciler previously recorded in `applied`.
pub fn compute_owned_pairing_diff(
    desired: &PairingDesired,
    actual: &PairingActual,
    applied: &PairingApplied,
) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for c in desired.collections.difference(&actual.collections) {
        ops.push(DiffOp::InstallCollection(c.clone()));
    }
    for c in actual
        .collections
        .intersection(&applied.collections)
        .filter(|c| !desired.collections.contains(*c))
    {
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
        .intersection(&applied.replicator_addresses)
        .filter(|r| !desired.replicator_addresses.contains(*r))
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

    #[test]
    fn owned_diff_does_not_teardown_unmanaged_actual() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["manual"]),
            replicator_addresses: s(&["/ip4/manual/p2p/p"]),
        };
        let applied = PairingApplied::default();
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    #[test]
    fn owned_diff_tears_down_only_applied_extras() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["manual", "managed"]),
            replicator_addresses: s(&["/ip4/manual/p2p/p", "/ip4/managed/p2p/p"]),
        };
        let applied = PairingApplied {
            collections: s(&["managed"]),
            replicator_addresses: s(&["/ip4/managed/p2p/p"]),
        };
        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownCollection("managed".into()),
                DiffOp::TeardownReplicator("/ip4/managed/p2p/p".into())
            ]
        );
    }
}
