use std::path::PathBuf;
use gents_p2p::p2p_reconcile::{DiffOp};
use gents_p2p::test_support::pairing_invariants::{check_liveness, check_safety, ObservedSnapshot};
use crate::support::pairing_conformance::runner::Harness;
use crate::support::pairing_conformance::scenario::Scenario;
    lean_pairing_reconcile_cases, lean_pairing_reconcile_sweep_scheduling_cases,
    LeanPairingReconcileSnapshot,

fn fixture_path(fixture: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pairing_scenarios")
        .join(fixture)
}

#[tokio::test]
async fn pairing_reconcile_scenarios_satisfy_safety_and_liveness() {
    for fixture in [
        "replicator_install_teardown.json",
        "read_failure_noop.json",
        "unmanaged_survival.json",
        "delete_after_restart.json",
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
    assert!(
        !matches!(ops.last(), Some(DiffOp::TeardownReplicator(_))),
        "reconverged pairing must not keep tearing down, got {ops:?}"
    );
}

