//! Safety and leads-to invariant evaluator for the pairing conformance harness.

use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::{
    compute_owned_pairing_diff, PairingActual as RuntimePairingActual,
};

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
    ConvergedSnapshotHasPendingOps,
    ConvergedStableDesiredFlapped,
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

        if check_liveness(snapshot) && !pending_owned_ops(snapshot).is_empty() {
            return Err(SafetyViolation::ConvergedSnapshotHasPendingOps);
        }

        if index == 0 {
            continue;
        }

        let previous = &history[index - 1];
        if check_liveness(previous)
            && previous.desired == snapshot.desired
            && !snapshot.read_failed
            && managed_or_desired_wiring_changed(previous, snapshot)
        {
            return Err(SafetyViolation::ConvergedStableDesiredFlapped);
        }

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

fn pending_owned_ops(snapshot: &ObservedSnapshot) -> Vec<gents::agent::p2p_reconcile::DiffOp> {
    compute_owned_pairing_diff(
        &snapshot.desired,
        &RuntimePairingActual {
            collections: snapshot.actual.collections.clone(),
            replicator_addresses: snapshot.actual.replicator_addresses.clone(),
            ..Default::default()
        },
        &snapshot.applied,
    )
}

fn managed_or_desired_wiring_changed(
    previous: &ObservedSnapshot,
    snapshot: &ObservedSnapshot,
) -> bool {
    previous.applied != snapshot.applied
        || previous.desired.collections.iter().any(|collection| {
            previous.actual.collections.contains(collection)
                != snapshot.actual.collections.contains(collection)
        })
        || previous.desired.replicator_addresses.iter().any(|address| {
            previous.actual.replicator_addresses.contains(address)
                != snapshot.actual.replicator_addresses.contains(address)
        })
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
        // The scope filter is part of the replicator identity. If managed
        // replicators are installed under a filter that no longer matches the
        // desired filter, a reinstall is still pending — not converged (Lean
        // `filter_change_forces_reinstall`).
        && (final_snapshot.applied.replicator_addresses.is_empty()
            || final_snapshot.applied.replicator_filter
                == final_snapshot.desired.replicator_filter)
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
                ..Default::default()
            },
            actual: PairingActual {
                collections: actual.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
                connected: !desired.is_empty(),
            },
            applied: PairingApplied {
                collections: applied.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
                ..Default::default()
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
                ..Default::default()
            },
            actual: PairingActual {
                collections: Default::default(),
                replicator_addresses: actual_replicators.iter().map(|s| s.to_string()).collect(),
                connected,
            },
            applied: PairingApplied {
                collections: Default::default(),
                replicator_addresses: applied_replicators.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
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

    #[test]
    fn converged_snapshot_has_no_pending_owned_ops() {
        let snapshot = snap(&["c1"], &["c1"], &["c1"]);
        assert_eq!(check_safety(&[snapshot]), Ok(()));
    }

    #[test]
    fn stable_desired_does_not_flap_after_convergence() {
        let before = snap(&["c1"], &["c1"], &["c1"]);
        let after = snap(&["c1"], &[], &[]);
        assert!(matches!(
            check_safety(&[before, after]),
            Err(SafetyViolation::ConvergedStableDesiredFlapped)
        ));
    }
}
