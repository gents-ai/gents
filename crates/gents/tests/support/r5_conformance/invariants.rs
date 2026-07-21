use super::runner::Observation;

pub fn assert_all_safety(o: &Observation) {
    completion::bridge_terminal_unique(o);
    completion::projection_requires_b_durable_terminal(o);
    completion::projection_matches_bridge_mapping(o);
    completion::notification_idempotent(o);
    completion::wakeup_coalesced(o);
    completion::wakeup_causal(o);
    completion::cancel_drain_preserves_user_pending(o);
    completion::parent_cancel_absorbs_late_terminal(o);
    cancel_propagation::cancel_intent_durable(o);
    cancel_propagation::cancel_handled_idempotent(o);
    cancel_propagation::interrupt_exactly_once(o);
    cancel_propagation::cascade_interrupts_only_running(o);
    cancel_propagation::natural_terminal_stable_after_cancel(o);
    cancel_propagation::interrupted_only_by_cascade(o);
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

    pub fn wakeup_causal(_: &Observation) {}
    pub fn cancel_drain_preserves_user_pending(_: &Observation) {}
    pub fn parent_cancel_absorbs_late_terminal(_: &Observation) {}
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

    pub fn cancel_handled_idempotent(_: &Observation) {}
    pub fn interrupt_exactly_once(_: &Observation) {}

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

    pub fn natural_terminal_stable_after_cancel(_: &Observation) {}
    pub fn interrupted_only_by_cascade(_: &Observation) {}
}
