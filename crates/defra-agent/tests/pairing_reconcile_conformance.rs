//! Entry test for the pairing reconcile conformance harness.

mod support;

use std::path::PathBuf;

use support::pairing_conformance::invariants::{check_liveness, check_safety, ObservedSnapshot};
use support::pairing_conformance::runner::Harness;
use support::pairing_conformance::scenario::Scenario;

#[tokio::test]
async fn install_teardown_happy_path_satisfies_safety_and_liveness() {
    std::env::set_var("DEFRA_AGENT_PAIRING_RECONCILE", "1");

    let scenario_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pairing_scenarios/install_teardown_happy_path.json");
    let scenario = Scenario::from_json_file(&scenario_path).expect("scenario parses");

    let mut harness = Harness::start_two_nodes().await.expect("harness starts");
    harness
        .run(&scenario)
        .await
        .expect("scenario runs to convergence");

    let history = harness.observation_history();
    check_safety(&history).expect("safety holds");
    let final_snapshot: &ObservedSnapshot = history.last().expect("non-empty history");
    assert!(check_liveness(final_snapshot), "convergence reached");
}
