//! Entry test for the pairing reconcile conformance harness.

use std::path::PathBuf;

use crate::support::pairing_conformance::invariants::{
    check_liveness, check_safety, ObservedSnapshot,
};
use crate::support::pairing_conformance::runner::Harness;
use crate::support::pairing_conformance::scenario::Scenario;

#[tokio::test]
async fn pairing_reconcile_scenarios_satisfy_safety_and_liveness() {
    for fixture in [
        "install_teardown_happy_path.json",
        "replicator_install_teardown.json",
        "read_failure_noop.json",
        "unmanaged_survival.json",
        "delete_after_restart.json",
    ] {
        let scenario_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pairing_scenarios");
        let scenario_path = scenario_path.join(fixture);
        let scenario = Scenario::from_json_file(&scenario_path).expect("scenario parses");

        let mut harness = Harness::start_two_nodes().await.expect("harness starts");
        harness
            .run(&scenario)
            .await
            .unwrap_or_else(|error| panic!("{fixture} runs to convergence: {error:?}"));

        let history = harness.observation_history();
        check_safety(&history).unwrap_or_else(|error| panic!("{fixture} safety holds: {error:?}"));
        let final_snapshot: &ObservedSnapshot = history.last().expect("non-empty history");
        assert!(
            check_liveness(final_snapshot),
            "{fixture} convergence reached"
        );
    }
}
