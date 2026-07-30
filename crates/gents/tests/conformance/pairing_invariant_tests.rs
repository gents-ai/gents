use crate::support::pairing_conformance::invariants::{
    check_liveness, check_safety, ObservedSnapshot, SafetyViolation,
};
use crate::support::pairing_conformance::{PairingActual, PairingApplied, PairingDesired};

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
