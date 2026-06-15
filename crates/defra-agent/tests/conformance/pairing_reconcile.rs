//! Entry test for the pairing reconcile conformance harness.

use std::path::PathBuf;

use defra_agent::agent::p2p_reconcile::DiffOp;

use crate::support::pairing_conformance::invariants::{
    check_liveness, check_safety, ObservedSnapshot,
};
use crate::support::pairing_conformance::runner::Harness;
use crate::support::pairing_conformance::scenario::Scenario;

fn fixture_path(fixture: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pairing_scenarios")
        .join(fixture)
}

#[tokio::test]
async fn pairing_reconcile_scenarios_satisfy_safety_and_liveness() {
    for fixture in [
        "install_teardown_happy_path.json",
        "replicator_install_teardown.json",
        "read_failure_noop.json",
        "unmanaged_survival.json",
        "delete_after_restart.json",
        "filter_change_reinstall.json",
    ] {
        let scenario_path = fixture_path(fixture);
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

/// Filter-change-reinstall scenario coverage (Finding #12): a converged pairing
/// whose scope filter changes must tear down the old filtered replicator and
/// install the new one on the same address — mirroring Lean
/// `PairingReconcile.filter_change_forces_reinstall`. This asserts the *op
/// shape*, so it would fail if the reconciler mutated the replicator in place.
#[tokio::test]
async fn filter_change_reinstalls_replicator() {
    const ADDR: &str = "/ip4/127.0.0.1/tcp/4103/p2p/peer-b";

    let scenario = Scenario::from_json_file(&fixture_path("filter_change_reinstall.json"))
        .expect("scenario parses");

    let mut harness = Harness::start_two_nodes().await.expect("harness starts");
    harness
        .run(&scenario)
        .await
        .expect("filter_change_reinstall runs to convergence");

    let history = harness.observation_history();
    check_safety(&history).expect("filter_change_reinstall safety holds");
    assert!(
        check_liveness(history.last().expect("non-empty history")),
        "filter_change_reinstall convergence reached"
    );

    let ops = harness.emitted_ops();
    // Initial install, then a teardown+install pair for the filter change.
    let teardown = ops
        .iter()
        .position(|op| matches!(op, DiffOp::TeardownReplicator(a) if a == ADDR))
        .expect("filter change must tear down the old filtered replicator");
    assert!(
        matches!(&ops[teardown + 1], DiffOp::InstallReplicator(a) if a == ADDR),
        "teardown must be immediately followed by reinstall on the same address, got {:?}",
        &ops[teardown..]
    );
    let installs = ops
        .iter()
        .filter(|op| matches!(op, DiffOp::InstallReplicator(a) if a == ADDR))
        .count();
    assert_eq!(
        installs, 2,
        "expected exactly two installs (initial + reinstall), got ops {ops:?}"
    );
    // Final reconcile is a no-op: no churn after reconvergence.
    assert!(
        !matches!(ops.last(), Some(DiffOp::TeardownReplicator(_))),
        "reconverged pairing must not keep tearing down, got {ops:?}"
    );
}
