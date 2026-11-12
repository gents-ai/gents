use std::collections::BTreeSet;

use crate::p2p_reconcile::{
    compute_owned_pairing_diff, to_replication_filters, PairingActual as RuntimePairingActual,
    PairingApplied, PairingDesired,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingActual {
    pub replicator_addresses: BTreeSet<String>,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSnapshot {
    pub desired: PairingDesired,
    pub actual: PairingActual,
    pub applied: PairingApplied,
    pub read_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyViolation {
    AppliedReplicatorWithoutPriorDesired { replicator: String },
    ReadFailureChangedActual,
    ReadFailureChangedApplied,
    ConvergedSnapshotHasPendingOps,
    ConvergedStableDesiredFlapped,
    UnmanagedReplicatorRemoved { replicator: String },
}

pub fn check_safety(history: &[ObservedSnapshot]) -> Result<(), SafetyViolation> {
    let mut desired_replicators_ever: BTreeSet<String> = BTreeSet::new();

    for (index, snapshot) in history.iter().enumerate() {
        desired_replicators_ever.extend(snapshot.desired.replicator_addresses.iter().cloned());

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

fn pending_owned_ops(snapshot: &ObservedSnapshot) -> Vec<crate::p2p_reconcile::DiffOp> {
    // Mirror the live filter observation so the runtime diff sees the same
    // route the harness installed instead of an artificial unfiltered route.
    let live_filter = to_replication_filters(&snapshot.applied.replicator_filter)
        .expect("conformance filters are representable");
    let replicator_filters = snapshot
        .actual
        .replicator_addresses
        .intersection(&snapshot.applied.replicator_addresses)
        .map(|address| (address.clone(), live_filter.clone()))
        .collect();
    compute_owned_pairing_diff(
        &snapshot.desired,
        &RuntimePairingActual {
            replicator_addresses: snapshot.actual.replicator_addresses.clone(),
            replicator_filters,
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
        || previous.desired.replicator_addresses.iter().any(|address| {
            previous.actual.replicator_addresses.contains(address)
                != snapshot.actual.replicator_addresses.contains(address)
        })
}

pub fn check_liveness(final_snapshot: &ObservedSnapshot) -> bool {
    final_snapshot
        .desired
        .replicator_addresses
        .is_subset(&final_snapshot.actual.replicator_addresses)
        && final_snapshot
            .applied
            .replicator_addresses
            .is_subset(&final_snapshot.desired.replicator_addresses)
        && (final_snapshot.applied.replicator_addresses.is_empty()
            || final_snapshot.applied.replicator_filter == final_snapshot.desired.replicator_filter)
        && (!final_snapshot.desired.has_wiring() || final_snapshot.actual.connected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(desired: &[&str], actual: &[&str], applied: &[&str]) -> ObservedSnapshot {
        ObservedSnapshot {
            desired: PairingDesired {
                replicator_addresses: desired.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            actual: PairingActual {
                replicator_addresses: actual.iter().map(|s| s.to_string()).collect(),
                connected: !desired.is_empty(),
            },
            applied: PairingApplied {
                replicator_addresses: applied.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            read_failed: false,
        }
    }

    #[test]
    fn safety_fails_on_phantom_applied() {
        let history = vec![snap(&[], &["c1"], &["c1"])];
        assert!(matches!(
            check_safety(&history),
            Err(SafetyViolation::AppliedReplicatorWithoutPriorDesired { .. })
        ));
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
    fn stable_desired_does_not_flap_after_convergence() {
        let before = snap(&["c1"], &["c1"], &["c1"]);
        let after = snap(&["c1"], &[], &[]);
        assert!(matches!(
            check_safety(&[before, after]),
            Err(SafetyViolation::ConvergedStableDesiredFlapped)
        ));
    }
}
