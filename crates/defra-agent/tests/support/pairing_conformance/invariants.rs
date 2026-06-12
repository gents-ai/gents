//! Safety and leads-to invariant evaluator for the pairing conformance harness.

use std::collections::BTreeSet;

use super::{PairingActual, PairingApplied, PairingDesired};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSnapshot {
    pub desired: PairingDesired,
    pub actual: PairingActual,
    pub applied: PairingApplied,
    pub read_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyViolation {
    AppliedCollectionWithoutPriorDesired { collection: String },
    AppliedReplicatorWithoutPriorDesired { replicator: String },
    ReadFailureChangedActual,
    ReadFailureChangedApplied,
    UnmanagedCollectionRemoved { collection: String },
    UnmanagedReplicatorRemoved { replicator: String },
}

pub fn check_safety(history: &[ObservedSnapshot]) -> Result<(), SafetyViolation> {
    let mut desired_collections_ever: BTreeSet<String> = BTreeSet::new();
    let mut desired_replicators_ever: BTreeSet<String> = BTreeSet::new();

    for (index, snapshot) in history.iter().enumerate() {
        desired_collections_ever.extend(snapshot.desired.collections.iter().cloned());
        desired_replicators_ever.extend(snapshot.desired.replicator_addresses.iter().cloned());

        for collection in snapshot.applied.collections.iter() {
            if !desired_collections_ever.contains(collection) {
                return Err(SafetyViolation::AppliedCollectionWithoutPriorDesired {
                    collection: collection.clone(),
                });
            }
        }
        for replicator in snapshot.applied.replicator_addresses.iter() {
            if !desired_replicators_ever.contains(replicator) {
                return Err(SafetyViolation::AppliedReplicatorWithoutPriorDesired {
                    replicator: replicator.clone(),
                });
            }
        }

        if index == 0 {
            continue;
        }

        let previous = &history[index - 1];
        if snapshot.read_failed {
            if snapshot.actual != previous.actual {
                return Err(SafetyViolation::ReadFailureChangedActual);
            }
            if snapshot.applied != previous.applied {
                return Err(SafetyViolation::ReadFailureChangedApplied);
            }
        }

        for collection in previous
            .actual
            .collections
            .difference(&previous.applied.collections)
        {
            if !snapshot.actual.collections.contains(collection) {
                return Err(SafetyViolation::UnmanagedCollectionRemoved {
                    collection: collection.clone(),
                });
            }
        }

        for replicator in previous
            .actual
            .replicator_addresses
            .difference(&previous.applied.replicator_addresses)
        {
            if !snapshot.actual.replicator_addresses.contains(replicator) {
                return Err(SafetyViolation::UnmanagedReplicatorRemoved {
                    replicator: replicator.clone(),
                });
            }
        }
    }
    Ok(())
}

pub fn check_liveness(final_snapshot: &ObservedSnapshot) -> bool {
    final_snapshot
        .desired
        .collections
        .is_subset(&final_snapshot.actual.collections)
        && final_snapshot
            .desired
            .replicator_addresses
            .is_subset(&final_snapshot.actual.replicator_addresses)
        && final_snapshot
            .applied
            .collections
            .is_subset(&final_snapshot.desired.collections)
        && final_snapshot
            .applied
            .replicator_addresses
            .is_subset(&final_snapshot.desired.replicator_addresses)
        && (!final_snapshot.desired.has_wiring() || final_snapshot.actual.connected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(desired: &[&str], actual: &[&str], applied: &[&str]) -> ObservedSnapshot {
        ObservedSnapshot {
            desired: PairingDesired {
                collections: desired.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
            },
            actual: PairingActual {
                collections: actual.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
                connected: !desired.is_empty(),
            },
            applied: PairingApplied {
                collections: applied.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
            },
            read_failed: false,
        }
    }

    fn snap_with_replicators(
        desired_replicators: &[&str],
        actual_replicators: &[&str],
        applied_replicators: &[&str],
        connected: bool,
    ) -> ObservedSnapshot {
        ObservedSnapshot {
            desired: PairingDesired {
                collections: Default::default(),
                replicator_addresses: desired_replicators.iter().map(|s| s.to_string()).collect(),
            },
            actual: PairingActual {
                collections: Default::default(),
                replicator_addresses: actual_replicators.iter().map(|s| s.to_string()).collect(),
                connected,
            },
            applied: PairingApplied {
                collections: Default::default(),
                replicator_addresses: applied_replicators.iter().map(|s| s.to_string()).collect(),
            },
            read_failed: false,
        }
    }

    #[test]
    fn safety_passes_when_actual_traces_to_desired() {
        let history = vec![snap(&["c1"], &[], &[]), snap(&["c1"], &["c1"], &["c1"])];
        assert_eq!(check_safety(&history), Ok(()));
    }

    #[test]
    fn safety_fails_on_phantom_applied() {
        let history = vec![snap(&[], &["c1"], &["c1"])];
        assert!(matches!(
            check_safety(&history),
            Err(SafetyViolation::AppliedCollectionWithoutPriorDesired { .. })
        ));
    }

    #[test]
    fn liveness_holds_when_desired_equals_actual() {
        let snapshot = snap(&["c1", "c2"], &["c1", "c2"], &["c1", "c2"]);
        assert!(check_liveness(&snapshot));
    }

    #[test]
    fn unmanaged_actual_survives_remove() {
        let history = vec![
            snap(&[], &["manual"], &[]),
            snap(&["managed"], &["manual", "managed"], &["managed"]),
            snap(&[], &["manual"], &[]),
        ];
        assert_eq!(check_safety(&history), Ok(()));
        assert!(check_liveness(history.last().unwrap()));
    }

    #[test]
    fn read_failure_must_not_change_actual_or_applied() {
        let before = snap(&["c1"], &["c1"], &["c1"]);
        let mut after = before.clone();
        after.read_failed = true;
        assert_eq!(check_safety(&[before, after]), Ok(()));
    }

    #[test]
    fn liveness_requires_connection_for_replicator_wiring() {
        let disconnected = snap_with_replicators(&["/ip4/1"], &["/ip4/1"], &["/ip4/1"], false);
        assert!(!check_liveness(&disconnected));

        let connected = snap_with_replicators(&["/ip4/1"], &["/ip4/1"], &["/ip4/1"], true);
        assert!(check_liveness(&connected));
    }
}
