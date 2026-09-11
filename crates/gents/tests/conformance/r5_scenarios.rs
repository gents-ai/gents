use std::path::PathBuf;

use gents_protocol::request_lifecycle::RequestLifecycleState;

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
    // Crash fixtures must cross a real process boundary; these checks fail if
    // Crash is deleted or restored as a no-op while the fixture still claims
    // a crash (false-green regression from the pre-fix harness).
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
    // Cross-principal background subagent: B crashes mid-execution, recovery
    // preserves the durable processing child, then a terminal failure projects
    // onto A's bridge exactly once (not R6 native-tool interrupt-on-restart).
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
    assert_eq!(child.lifecycle_state, RequestLifecycleState::Failed);
    assert!(
        last.b_process_generation >= 1,
        "B must have crossed at least one process crash boundary"
    );
    // Projection side effects are durable and unique.
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
    // A crashes while waiting on a background child; child terminals that
    // land while A is down (and after a second crash window) must each
    // project exactly once onto the durable parent bridge.
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
        "restart recovery must enqueue one coalesced wake key per parent session"
    );
}

#[tokio::test]
async fn r5_partition_during_cancel() {
    let history = run_scenario("partition_during_cancel.json").await;
    // Before the fixture terminalizes the child, the mirror must latch the exact intent.
    let mirrored = history
        .iter()
        .find(|snapshot| {
            snapshot.b_child_requests.iter().any(|child| {
                child.request_id == "child-req-cancel"
                    && child.lifecycle_state == RequestLifecycleState::Processing
                    && child.interrupt_requested_at.is_some()
            })
        })
        .expect("cancel mirror must interrupt the still-processing child");
    let child = mirrored
        .b_child_requests
        .iter()
        .find(|child| child.request_id == "child-req-cancel")
        .unwrap();
    let intent = mirrored
        .b_bridge_rows
        .iter()
        .find(|bridge| bridge.tool_call_id == "tool-call-cancel")
        .unwrap();
    assert_eq!(
        child.interrupt_requested_at,
        intent.cancel_cascade_intent_at
    );
    // Observe the real ack owner latching a stuck intent, then clearing it after replication.
    let stuck_observed = history.iter().any(|o| {
        o.a_bridge_rows.iter().any(|b| {
            b.tool_call_id == "tool-call-cancel"
                && b.cancel_pending_remote_ack == Some(true)
                && b.stuck_since.is_some()
        })
    });
    assert!(
        stuck_observed,
        "aging the cancel intent past the stuck threshold must latch stuck_since on the pending bridge"
    );
    let last = history.last().expect("non-empty history");
    let bridge = last
        .a_bridge_rows
        .iter()
        .find(|b| b.tool_call_id == "tool-call-cancel")
        .expect("partition bridge on A");
    assert_eq!(bridge.lifecycle_state, "cancelled");
    assert_eq!(
        bridge.cancel_pending_remote_ack,
        Some(false),
        "the child's interrupted terminal must ack and clear the pending remote cancel"
    );
}

#[tokio::test]
async fn r5_multi_completion_coalesces_wake() {
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
        "multi-completion delivery must coalesce wakes under one queue key"
    );
}
