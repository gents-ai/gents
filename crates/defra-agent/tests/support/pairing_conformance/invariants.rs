//! Safety and leads-to invariant evaluator for the pairing conformance harness.

use std::collections::BTreeSet;

use super::{PairingActual, PairingDesired};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSnapshot {
    pub desired: PairingDesired,
    pub actual: PairingActual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyViolation {
    ActualWithoutPriorDesired { collection: String },
}

pub fn check_safety(history: &[ObservedSnapshot]) -> Result<(), SafetyViolation> {
    let mut all_desired_ever: BTreeSet<String> = BTreeSet::new();
    for snapshot in history {
        all_desired_ever.extend(snapshot.desired.collections.iter().cloned());
        for collection in snapshot.actual.collections.iter() {
            if !all_desired_ever.contains(collection) {
                return Err(SafetyViolation::ActualWithoutPriorDesired {
                    collection: collection.clone(),
                });
            }
        }
    }
    Ok(())
}

pub fn check_liveness(final_snapshot: &ObservedSnapshot) -> bool {
    final_snapshot.desired.collections == final_snapshot.actual.collections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(desired: &[&str], actual: &[&str]) -> ObservedSnapshot {
        ObservedSnapshot {
            desired: PairingDesired {
                collections: desired.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
            },
            actual: PairingActual {
                collections: actual.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
            },
        }
    }

    #[test]
    fn safety_passes_when_actual_traces_to_desired() {
        let history = vec![snap(&["c1"], &[]), snap(&["c1"], &["c1"])];
        assert_eq!(check_safety(&history), Ok(()));
    }

    #[test]
    fn safety_fails_on_phantom_actual() {
        let history = vec![snap(&[], &["c1"])];
        assert!(matches!(
            check_safety(&history),
            Err(SafetyViolation::ActualWithoutPriorDesired { .. })
        ));
    }

    #[test]
    fn liveness_holds_when_desired_equals_actual() {
        let snapshot = snap(&["c1", "c2"], &["c1", "c2"]);
        assert!(check_liveness(&snapshot));
    }
}
