use crate::support::pairing_conformance::invariants::{
    check_liveness, check_safety, ObservedSnapshot, SafetyViolation,
};
use crate::support::pairing_conformance::{PairingActual, PairingApplied, PairingDesired};

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
