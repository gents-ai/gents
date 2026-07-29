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
                    "durable child terminal did not settle onto bridge {}",
                    bridge.tool_call_id
                );
            }
        }
        if bridge.cancel_cascade_intent_at.is_some() {
            let Some(child) = last.child_for_bridge(bridge) else {
                panic!("cancel intent {} has no child", bridge.tool_call_id);
            };
            if !(child.interrupt_requested_at.is_some() || child.is_terminal()) {
                panic!(
                    "cancel intent {} did not interrupt or absorb into terminal child",
                    bridge.tool_call_id
                );
            }
        }
    }
}

/// Crash/restart boundary checks. These bind the harness `Crash` action to an
/// observable process-identity change (TLA `crashCount` / CrashA/CrashB) while
/// durable bridge and child rows remain loadable from the reopened store.
pub fn assert_crash_boundary(history: &[Observation]) {
    crash::assert_crashes_observed(history);
    crash::assert_durable_rows_survive_crash(history);
}

pub mod crash {
    use super::Observation;

    /// Per-snapshot: if this observation followed a Crash action, that node's
    /// process generation must be strictly positive. A no-op Crash leaves
    /// generation at 0 and fails this check once any crash action is claimed.
    pub fn process_generation_advances_on_crash(o: &Observation) {
        let Some(crashed) = o.crashed_node.as_deref() else {
            return;
        };
        match crashed {
            "A" => assert!(
                o.a_process_generation > 0,
                "Crash(A) left a_process_generation at 0 (no-op crash)"
            ),
            "B" => assert!(
                o.b_process_generation > 0,
                "Crash(B) left b_process_generation at 0 (no-op crash)"
            ),
            other => panic!("unknown crashed node {other}"),
        }
    }

    /// History-level: every Crash action must advance that node's generation
    /// relative to the immediately preceding observation. Deleting the Crash
    /// arm or restoring a no-op fails this.
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
                    "Crash(A) did not advance process generation ({} -> {}); \
                     false-green no-op Crash is not allowed",
                    prev.a_process_generation,
                    curr.a_process_generation
                ),
                "B" => assert!(
                    curr.b_process_generation > prev.b_process_generation,
                    "Crash(B) did not advance process generation ({} -> {}); \
                     false-green no-op Crash is not allowed",
                    prev.b_process_generation,
                    curr.b_process_generation
                ),
                other => panic!("unknown crashed node {other}"),
            }
        }
        assert!(
            crash_count > 0,
            "crash scenario history has no Crash observations; fixture must cross the crash boundary"
        );
    }

    /// Durable AgentToolCall bridges and child AgentRequest rows present just
    /// before a Crash must still be loadable immediately after reopen. This is
    /// the durable-vs-volatile split: process identity changes, rows do not
    /// disappear with the process.
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
                other => panic!("unknown crashed node {other}"),
            };
            for bridge in prev_bridges {
                assert!(
                    curr_bridges
                        .iter()
                        .any(|b| b.tool_call_id == bridge.tool_call_id
                            && b.lifecycle_state == bridge.lifecycle_state
                            && b.child_request_id == bridge.child_request_id),
                    "Crash({crashed}) lost durable bridge {}",
                    bridge.tool_call_id
                );
            }
            for child in prev_children {
                assert!(
                    curr_children
                        .iter()
                        .any(|c| c.request_id == child.request_id
                            && c.lifecycle_state == child.lifecycle_state),
                    "Crash({crashed}) lost durable child request {}",
                    child.request_id
                );
            }
        }
    }
}

pub mod completion {
    use super::Observation;

    pub fn bridge_terminal_unique(o: &Observation) {
        for bridge in &o.a_bridge_rows {
            assert!(
                matches!(
                    bridge.lifecycle_state.as_str(),
                    "running" | "completed" | "failed" | "timedOut" | "cancelled"
                ),
                "bridge {} has invalid lifecycle_state {}",
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
                    "bridge {} terminalized without durable child terminal",
                    bridge.tool_call_id
                );
            }
        }
    }

    pub fn projection_matches_bridge_mapping(o: &Observation) {
        for bridge in &o.a_bridge_rows {
            if let Some(child) = o.child_for_bridge(bridge) {
                if child.lifecycle_state == "interrupted" {
                    assert!(
                        bridge.lifecycle_state == "running"
                            || bridge.lifecycle_state == "cancelled",
                        "interrupted child should map to cancelled bridge"
                    );
                }
            }
        }
    }

    pub fn notification_idempotent(o: &Observation) {
        let mut seen = std::collections::HashSet::new();
        for note in &o.subagent_notifications {
            assert!(seen.insert(note.clone()), "duplicate notification {note}");
        }
    }

    pub fn wakeup_coalesced(o: &Observation) {
        let mut seen = std::collections::HashSet::new();
        for key in &o.background_wakeup_keys {
            assert!(seen.insert(key.clone()), "duplicate wakeup key {key}");
        }
    }
}

pub mod cancel_propagation {
    use super::Observation;

    pub fn cancel_intent_durable(o: &Observation) {
        for bridge in &o.a_bridge_rows {
            if bridge.cancel_pending_remote_ack == Some(true) {
                assert!(
                    bridge.cancel_cascade_intent_at.is_some(),
                    "pending remote ack without durable cancel intent"
                );
            }
        }
    }

    pub fn cascade_interrupts_only_running(o: &Observation) {
        for child in &o.b_child_requests {
            if child.interrupt_requested_at.is_some() {
                assert!(
                    !matches!(
                        child.lifecycle_state.as_str(),
                        "completed" | "failed" | "dead" | "superseded"
                    ),
                    "natural terminal child {} was interrupted",
                    child.request_id
                );
            }
        }
    }
}
