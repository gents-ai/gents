use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::lean_vocab_test::lean_r5_scenario_cases;
use crate::support::r5_conformance::invariants;
use crate::support::r5_conformance::runner::Observation;
use crate::support::r5_conformance::scenario::ModeledScenario;
use crate::support::r5_conformance::Harness;

#[tokio::test]
async fn generated_r5_p2p_crash_reopens_same_durable_peer_identity() {
    let mut db = crate::support::test_p2p_db("r5-crash-peer-identity").await;
    let (before_peer, _) =
        crate::support::enrollment::wait_for_peer_identity(db.node.as_ref()).await;
    db.simulate_process_crash()
        .await
        .expect("reopen R5 P2P store after crash");
    let (after_peer, _) =
        crate::support::enrollment::wait_for_peer_identity(db.node.as_ref()).await;
    assert_eq!(
        before_peer, after_peer,
        "durable R5 peer identity changed on restart"
    );
    assert_eq!(db.process_generation, 1);
}

#[test]
fn generated_r5_actions_decode_without_handwritten_fixture_defaults() {
    let cases = lean_r5_scenario_cases();
    assert_eq!(cases.len(), 6, "the Lean owner exports every R5 scenario");
    for raw in cases {
        let case: ModeledScenario = serde_json::from_value(raw.clone())
            .unwrap_or_else(|error| panic!("R5 modeled action decode failed: {error}: {raw}"));
        assert!(!case.name.is_empty());
        assert!(!case.actions.is_empty());
    }
}

#[tokio::test]
async fn generated_r5_happy_path_uses_native_owners_and_physical_p2p_docs() {
    run_generated_case("happy_path").await;
}

#[tokio::test]
async fn generated_r5_b_crash_mid_execution_uses_native_recovery() {
    let history = run_generated_case("b_crash_mid_execution").await;
    invariants::assert_crash_boundary(&history);
}

#[tokio::test]
async fn generated_r5_a_crash_mid_wait_uses_native_recovery() {
    let history = run_generated_case("a_crash_mid_wait").await;
    invariants::assert_crash_boundary(&history);
}

#[tokio::test]
async fn generated_r5_partition_during_cancel_uses_native_mirror() {
    let history = run_generated_case("partition_during_cancel").await;
    let mirrored = history
        .iter()
        .find(|snapshot| {
            snapshot.b_child_requests.iter().any(|child| {
                child.lifecycle_state == RequestLifecycleState::Processing
                    && child.interrupt_requested_at.is_some()
            })
        })
        .expect("modeled MirrorCancel must interrupt the still-processing child");
    let mirrored_child = mirrored
        .b_child_requests
        .iter()
        .find(|child| child.interrupt_requested_at.is_some())
        .expect("mirrored child");
    let mirrored_bridge = mirrored
        .b_bridge_rows
        .iter()
        .find(|bridge| {
            bridge.child_request_id.as_deref() == Some(mirrored_child.request_id.as_str())
        })
        .expect("exact replicated cancel bridge");
    assert_eq!(
        mirrored_child.interrupt_requested_at, mirrored_bridge.cancel_cascade_intent_at,
        "B mirror must latch the exact coordinator cancel intent"
    );
    assert!(
        history
            .iter()
            .any(|snapshot| snapshot.a_bridge_rows.iter().any(|bridge| {
                bridge.cancel_pending_remote_ack == Some(true) && bridge.stuck_since.is_some()
            })),
        "modeled clock advance and first ObserveCancelAck must mark pending cancel stuck"
    );
    let last = history.last().expect("R5 cancel history");
    assert!(
        last.a_bridge_rows.iter().any(|bridge| {
            bridge.lifecycle_state == "cancelled" && bridge.cancel_pending_remote_ack == Some(false)
        }),
        "terminal child and final ObserveCancelAck must clear pending remote ack"
    );
}

#[tokio::test]
async fn generated_r5_multi_completion_coalesces_native_wake() {
    run_generated_case("multi_completion_coalesce").await;
}

#[tokio::test]
async fn generated_r5_remote_depth_ceiling_uses_native_admission() {
    run_generated_case("remote_depth_ceiling").await;
}

async fn run_generated_case(name: &str) -> Vec<Observation> {
    let raw = lean_r5_scenario_cases()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("Lean does not export R5 scenario {name}"));
    let case: ModeledScenario = serde_json::from_value(raw.clone()).expect("decode modeled R5");
    let mut harness = Harness::start_generated(&case)
        .await
        .expect("start generated R5 peers");
    harness
        .run_modeled(&case)
        .await
        .expect("execute modeled R5 actions");
    let history = harness.observation_history();
    for snapshot in &history {
        invariants::assert_all_safety(snapshot);
    }
    invariants::assert_liveness_after_convergence(&history);
    let last = history.last().expect("generated R5 history");
    let (modeled_notifications, modeled_wakes) = harness
        .modeled_completion_keys()
        .expect("modeled R5 accepted bridges have exact completion identities");
    assert_eq!(
        last.subagent_notifications
            .iter()
            .filter(|key| modeled_notifications.contains(*key))
            .count(),
        case.expected_notifications,
        "modeled R5 notification count disagrees with physical terminal snapshot: {last:?}"
    );
    assert_eq!(
        last.background_wakeup_keys
            .iter()
            .filter(|key| modeled_wakes.contains(*key))
            .count(),
        case.expected_wakes,
        "modeled R5 wake count disagrees with exact parent sessions: {last:?}"
    );
    assert_eq!(
        last.a_rejected_spawn_invocation_ids.len(),
        case.expected_rejected_invocations,
        "modeled depth-rejected invocations must be durable failed tool rows"
    );
    assert_eq!(last.a_process_generation, case.expected_a_generation);
    assert_eq!(last.b_process_generation, case.expected_b_generation);
    for expected in &case.expected_a_bridges {
        let bridge = last
            .a_bridge_rows
            .iter()
            .find(|bridge| bridge.tool_call_id == expected.tool)
            .unwrap_or_else(|| panic!("modeled bridge {} missing from A", expected.tool));
        assert_eq!(
            bridge.lifecycle_state, expected.state,
            "Lean-derived final state of bridge {}",
            expected.tool
        );
        let physical_child = harness
            .generated_child_request_id(&expected.child)
            .expect("modeled bridge has exact physical child");
        assert_eq!(bridge.child_request_id.as_deref(), Some(physical_child));
    }
    for expected in &case.expected_b_children {
        let physical_child = harness
            .generated_child_request_id(&expected.child)
            .expect("modeled B child has exact physical reservation");
        let child = last
            .b_child_requests
            .iter()
            .find(|child| child.request_id == physical_child)
            .unwrap_or_else(|| panic!("modeled B child {} is missing", expected.child));
        assert_eq!(
            expected.terminal.as_deref(),
            child
                .lifecycle_state
                .is_terminal()
                .then_some(child.lifecycle_state.as_str()),
            "Lean-derived terminal state of B child {}",
            expected.child
        );
        assert_eq!(
            child.interrupt_requested_at.is_some(),
            expected.interrupt_requested,
            "Lean-derived interrupt fact of B child {}",
            expected.child
        );
    }
    history.to_vec()
}
