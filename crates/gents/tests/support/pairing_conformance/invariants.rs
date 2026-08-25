use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::{
    compute_owned_pairing_diff, to_replication_filters, PairingActual as RuntimePairingActual,
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
    // The conformance harness applies every successful replicator install with
    // the desired filter and records that identity in `PairingApplied`. Mirror
    // that live filter observation here so the runtime diff sees the same
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
            collections: snapshot.actual.collections.clone(),
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
        && (final_snapshot.applied.replicator_addresses.is_empty()
            || final_snapshot.applied.replicator_filter == final_snapshot.desired.replicator_filter)
        && (!final_snapshot.desired.has_wiring() || final_snapshot.actual.connected)
}
