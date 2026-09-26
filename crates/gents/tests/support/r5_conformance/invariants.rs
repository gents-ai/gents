use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::runner::Observation;

pub fn assert_all_safety(o: &Observation) {
    completion::bridge_terminal_unique(o);
    completion::projection_requires_b_durable_terminal(o);
    completion::projection_matches_bridge_mapping(o);
    completion::notification_idempotent(o);
    completion::wakeup_coalesced(o);
    cancel_propagation::cancel_intent_durable(o);
    cancel_propagation::cascade_interrupts_only_running(o);
    crash::process_generation_advances_on_crash(o);
}

pub fn assert_liveness_after_convergence(history: &[Observation]) {
    let Some(last) = history.last() else {
        panic!("R5 observation history must be non-empty");
    };
    for bridge in &last.a_bridge_rows {
        if let Some(child) = last.child_for_bridge(bridge) {
            if child.is_terminal() && bridge.lifecycle_state == "running" {
                panic!(
                    "[{}] durable child terminal did not settle onto bridge {}",
                    last.origin, bridge.tool_call_id
                );
            }
        }
        if bridge.cancel_cascade_intent_at.is_some() {
            let Some(child) = last.child_for_bridge(bridge) else {
                panic!(
                    "[{}] cancel intent {} has no child",
                    last.origin, bridge.tool_call_id
                );
            };
            if !(child.interrupt_requested_at.is_some() || child.is_terminal()) {
                panic!(
                    "[{}] cancel intent {} did not interrupt or absorb into terminal child",
                    last.origin, bridge.tool_call_id
                );
            }
        }
    }
}

pub fn assert_crash_boundary(history: &[Observation]) {
    crash::assert_crashes_observed(history);
    crash::assert_durable_rows_survive_crash(history);
}

pub mod crash {
    use super::Observation;

    pub fn process_generation_advances_on_crash(o: &Observation) {
        let Some(crashed) = o.crashed_node.as_deref() else {
            return;
        };
        match crashed {
            "A" => assert!(
                o.a_process_generation > 0,
                "[{}] Crash(A) left a_process_generation at 0 (no-op crash)",
                o.origin
            ),
            "B" => assert!(
                o.b_process_generation > 0,
                "[{}] Crash(B) left b_process_generation at 0 (no-op crash)",
                o.origin
            ),
            other => panic!("[{}] unknown crashed node {other}", o.origin),
        }
    }

    pub fn assert_crashes_observed(history: &[Observation]) {
        let mut crash_count = 0usize;
        for window in history.windows(2) {
            let prev = &window[0];
            let curr = &window[1];
            let Some(crashed) = curr.crashed_node.as_deref() else {
                continue;
            };
            crash_count += 1;
            match crashed {
                "A" => assert!(
                    curr.a_process_generation > prev.a_process_generation,
                    "[{}] Crash(A) did not advance process generation ({} -> {}); \
                     false-green no-op Crash is not allowed; previous sample [{}]",
                    curr.origin,
                    prev.a_process_generation,
                    curr.a_process_generation,
                    prev.origin
                ),
                "B" => assert!(
                    curr.b_process_generation > prev.b_process_generation,
                    "[{}] Crash(B) did not advance process generation ({} -> {}); \
                     false-green no-op Crash is not allowed; previous sample [{}]",
                    curr.origin,
                    prev.b_process_generation,
                    curr.b_process_generation,
                    prev.origin
                ),
                other => panic!("[{}] unknown crashed node {other}", curr.origin),
            }
        }
        assert!(
            crash_count > 0,
            "crash scenario history has no Crash observations; fixture must cross the crash boundary"
        );
    }

    pub fn assert_durable_rows_survive_crash(history: &[Observation]) {
        for window in history.windows(2) {
            let prev = &window[0];
            let curr = &window[1];
            let Some(crashed) = curr.crashed_node.as_deref() else {
                continue;
            };
            let (prev_bridges, curr_bridges, prev_children, curr_children) = match crashed {
                "A" => (
                    &prev.a_bridge_rows,
                    &curr.a_bridge_rows,
                    &prev.a_child_requests,
                    &curr.a_child_requests,
                ),
                "B" => (
                    &prev.b_bridge_rows,
                    &curr.b_bridge_rows,
                    &prev.b_child_requests,
                    &curr.b_child_requests,
                ),
                other => panic!("[{}] unknown crashed node {other}", curr.origin),
            };
            for bridge in prev_bridges {
                assert!(
                    curr_bridges
                        .iter()
                        .any(|b| b.tool_call_id == bridge.tool_call_id
                            && b.lifecycle_state == bridge.lifecycle_state
                            && b.child_request_id == bridge.child_request_id),
                    "[{}] Crash({crashed}) lost durable bridge {}; before={bridge:?}, after={curr_bridges:?}; previous sample [{}]",
                    curr.origin,
                    bridge.tool_call_id,
                    prev.origin
                );
            }
            for child in prev_children {
                assert!(
                    curr_children
                        .iter()
                        .any(|c| c.request_id == child.request_id
                            && c.lifecycle_state == child.lifecycle_state),
                    "[{}] Crash({crashed}) lost durable child request {}: before={child:?}, after={curr_children:?}; previous sample [{}]",
                    curr.origin,
                    child.request_id,
                    prev.origin
                );
            }
        }
    }
}

pub mod completion {
    use super::{Observation, RequestLifecycleState};

    pub fn bridge_terminal_unique(o: &Observation) {
        for bridge in &o.a_bridge_rows {
            assert!(
                matches!(
                    bridge.lifecycle_state.as_str(),
                    "running" | "completed" | "failed" | "timedOut" | "cancelled"
                ),
                "[{}] bridge {} has invalid lifecycle_state {}",
                o.origin,
                bridge.tool_call_id,
                bridge.lifecycle_state
            );
        }
    }

    pub fn projection_requires_b_durable_terminal(o: &Observation) {
        for bridge in &o.a_bridge_rows {
            if bridge.lifecycle_state != "running" && bridge.cancel_cascade_intent_at.is_none() {
                let child = o.child_for_bridge(bridge);
                assert!(
                    child.is_some_and(|child| child.is_terminal()),
                    "[{}] bridge {} terminalized without durable child terminal",
                    o.origin,
                    bridge.tool_call_id
                );
            }
        }
    }

    pub fn projection_matches_bridge_mapping(o: &Observation) {
        for bridge in &o.a_bridge_rows {
            if let Some(child) = o.child_for_bridge(bridge) {
                if child.lifecycle_state == RequestLifecycleState::Interrupted {
                    assert!(
                        bridge.lifecycle_state == "running"
                            || bridge.lifecycle_state == "cancelled",
                        "[{}] interrupted child should map to cancelled bridge: {bridge:?}",
                        o.origin
                    );
                }
            }
        }
    }

    pub fn notification_idempotent(o: &Observation) {
        let mut seen = std::collections::HashSet::new();
        for note in &o.subagent_notifications {
            assert!(
                seen.insert(note.clone()),
                "[{}] duplicate notification {note}",
                o.origin
            );
        }
    }

    pub fn wakeup_coalesced(o: &Observation) {
        let mut seen = std::collections::HashSet::new();
        for key in &o.background_wakeup_keys {
            assert!(
                seen.insert(key.clone()),
                "[{}] duplicate wakeup key {key}",
                o.origin
            );
        }
    }
}

pub mod cancel_propagation {
    use super::{Observation, RequestLifecycleState};

    pub fn cancel_intent_durable(o: &Observation) {
        for bridge in &o.a_bridge_rows {
            if bridge.cancel_pending_remote_ack == Some(true) {
                assert!(
                    bridge.cancel_cascade_intent_at.is_some(),
                    "[{}] pending remote ack without durable cancel intent: {bridge:?}",
                    o.origin
                );
            }
        }
    }

    /// A child that was actually interrupted legitimately settles into the
    /// `Interrupted` terminal state — that is the intended outcome of the
    /// interrupt, not a violation. The banned set is therefore "terminal via
    /// some other, natural path" (completed/failed/dead/superseded): terminal
    /// states reachable without an interrupt ever landing. `Interrupted`
    /// itself is `RequestLifecycleState::is_terminal() == true` but is
    /// explicitly exempted here.
    pub fn cascade_interrupts_only_running(o: &Observation) {
        for child in &o.b_child_requests {
            if child.interrupt_requested_at.is_some() {
                let naturally_terminal = child.lifecycle_state.is_terminal()
                    && child.lifecycle_state != RequestLifecycleState::Interrupted;
                assert!(
                    !naturally_terminal,
                    "[{}] natural terminal child {} was interrupted (lifecycle_state={})",
                    o.origin, child.request_id, child.lifecycle_state
                );
            }
        }
    }
}
