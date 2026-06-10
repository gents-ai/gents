use std::path::PathBuf;

use crate::support::r5_conformance::invariants;
use crate::support::r5_conformance::runner::Observation;
use crate::support::r5_conformance::{Harness, Scenario};

async fn run_scenario(filename: &str) -> Vec<Observation> {
    let path: PathBuf = ["tests", "fixtures", "r5_scenarios", filename]
        .iter()
        .collect();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing scenario {}", path.display()));
    let scenario: Scenario = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("invalid scenario {}: {e}", path.display()));
    let mut harness = Harness::start_two_nodes().await.expect("harness start");
    harness.run(&scenario).await.expect("scenario run");
    let history = harness.observation_history();
    for snapshot in &history {
        invariants::assert_all_safety(&snapshot);
    }
    invariants::assert_liveness_after_convergence(&history);
    history
}

#[tokio::test]
async fn r5_happy_path() {
    run_scenario("happy_path.json").await;
}

#[tokio::test]
async fn r5_b_crash_mid_execution() {
    run_scenario("b_crash_mid_execution.json").await;
}

#[tokio::test]
async fn r5_a_crash_mid_wait() {
    run_scenario("a_crash_mid_wait.json").await;
}

#[tokio::test]
async fn r5_partition_during_cancel() {
    run_scenario("partition_during_cancel.json").await;
}

#[tokio::test]
async fn r5_multi_completion_coalesce() {
    let history = run_scenario("multi_completion_coalesce.json").await;
    let last = history.last().expect("non-empty history");
    assert_eq!(
        last.subagent_notifications.len(),
        2,
        "multi-completion scenario should emit one notification per child"
    );
    assert_eq!(
        last.background_wakeup_keys.len(),
        1,
        "multi-completion scenario should coalesce wakeups under one queue key"
    );
}
