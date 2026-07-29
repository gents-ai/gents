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

async fn run_crash_scenario(filename: &str) -> Vec<Observation> {
    let history = run_scenario(filename).await;
    invariants::assert_crash_boundary(&history);
    history
}

#[tokio::test]
async fn r5_happy_path() {
    run_scenario("happy_path.json").await;
}

#[tokio::test]
async fn r5_b_crash_mid_execution() {
    let history = run_crash_scenario("b_crash_mid_execution.json").await;
    let last = history.last().expect("non-empty history");
    let bridge = last
        .a_bridge_rows
        .iter()
        .find(|b| b.tool_call_id == "tool-call-b-crash")
        .expect("parent bridge on A");
    assert_eq!(
        bridge.lifecycle_state, "failed",
        "B-crash scenario must project child failure onto parent bridge"
    );
    let child = last
        .child_for_bridge(bridge)
        .expect("child row for B-crash bridge");
    assert_eq!(child.lifecycle_state, "failed");
    assert!(
        last.b_process_generation >= 1,
        "B must have crossed at least one process crash boundary"
    );
    assert_eq!(
        last.subagent_notifications.len(),
        1,
        "failed child projects one subagent notification"
    );
}

#[tokio::test]
async fn r5_a_crash_mid_wait() {
    let history = run_crash_scenario("a_crash_mid_wait.json").await;
    let last = history.last().expect("non-empty history");
    let before = last
        .a_bridge_rows
        .iter()
        .find(|b| b.tool_call_id == "tool-call-a-crash-before")
        .expect("pre-crash bridge");
    let after = last
        .a_bridge_rows
        .iter()
        .find(|b| b.tool_call_id == "tool-call-a-crash-after")
        .expect("post-crash bridge");
    assert_eq!(before.lifecycle_state, "completed");
    assert_eq!(after.lifecycle_state, "completed");
    assert!(
        last.a_process_generation >= 2,
        "A must have crashed twice (generation={})",
        last.a_process_generation
    );
    assert_eq!(
        last.subagent_notifications.len(),
        2,
        "each completed background child projects one notification"
    );
    assert_eq!(
        last.background_wakeup_keys.len(),
        2,
        "one coalesced wakeup key per parent session"
    );
}

#[tokio::test]
async fn r5_partition_during_cancel() {
    run_scenario("partition_during_cancel.json").await;
}

#[tokio::test]
async fn r5_multi_completion_delivery_creates_no_agent_request() {
    let history = run_scenario("multi_completion_delivery.json").await;
    let last = history.last().expect("non-empty history");
    assert_eq!(
        last.subagent_notifications.len(),
        2,
        "multi-completion scenario should emit one notification per child"
    );
    assert_eq!(
        last.background_wakeup_keys.len(),
        0,
        "multi-completion delivery must not create a background wake request"
    );
}
