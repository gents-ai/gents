use super::*;

#[test]
fn tool_call_transitions_match_lean_contract() {
    assert_lean_transition_is_legal("ToolCall", "pending", "running");
    assert_lean_transition_is_legal("ToolCall", "pending", "failed");
    assert_lean_transition_is_legal("ToolCall", "pending", "cancelled");
    assert_lean_transition_is_legal("ToolCall", "running", "completed");
    assert_lean_transition_is_legal("ToolCall", "running", "failed");
    assert_lean_transition_is_legal("ToolCall", "running", "timedOut");
    assert_lean_transition_is_legal("ToolCall", "running", "cancelled");

    assert_lean_transition_is_legal("ToolCall", "pending", "awaitingApproval");
    assert_lean_transition_is_legal("ToolCall", "awaitingApproval", "awaitingApproval");
    assert_lean_transition_is_legal("ToolCall", "awaitingApproval", "running");
    assert_lean_transition_is_legal("ToolCall", "awaitingApproval", "failed");
    assert_lean_transition_is_legal("ToolCall", "awaitingApproval", "cancelled");
    assert_lean_transition_is_legal("ToolCall", "awaitingApproval", "timedOut");
    assert_lean_transition_is_illegal("ToolCall", "running", "awaitingApproval");
    assert_lean_transition_is_illegal("ToolCall", "completed", "awaitingApproval");
    assert_lean_transition_is_illegal("ToolCall", "awaitingApproval", "completed");
    assert_lean_transition_is_illegal("ToolCall", "awaitingApproval", "pending");

    assert_lean_transition_is_illegal("ToolCall", "completed", "running");
    assert_lean_transition_is_illegal("ToolCall", "failed", "running");
    assert_lean_transition_is_illegal("ToolCall", "timedOut", "running");
    assert_lean_transition_is_illegal("ToolCall", "cancelled", "running");
}

#[test]
pub(super) fn lean_emits_await_mode_vocabulary() {
    use gents::tool_call_lifecycle::AwaitMode;

    let machine = lean_state_machine_contract("AwaitMode");
    let mut rust_vocab: Vec<String> = AwaitMode::ALL
        .iter()
        .map(|m| m.as_str().to_string())
        .collect();
    rust_vocab.sort();
    let mut lean_vocab = machine.states.clone();
    lean_vocab.sort();
    assert_eq!(
        lean_vocab, rust_vocab,
        "AwaitMode vocabulary divergence between Lean and Rust"
    );
}

#[test]
pub(super) fn lean_emits_cancel_policy_vocabulary() {
    use gents::tool_call_lifecycle::CancelPolicy;

    let machine = lean_state_machine_contract("CancelPolicy");
    let mut rust_vocab: Vec<String> = CancelPolicy::ALL
        .iter()
        .map(|p| p.as_str().to_string())
        .collect();
    rust_vocab.sort();
    let mut lean_vocab = machine.states.clone();
    lean_vocab.sort();
    assert_eq!(
        lean_vocab, rust_vocab,
        "CancelPolicy vocabulary divergence between Lean and Rust"
    );
}

#[test]
pub(super) fn lean_emits_child_terminal_vocabulary_and_projections() {
    use gents::tool_call_lifecycle::ChildTerminal;

    let machine = lean_state_machine_contract("ChildTerminal");

    let mut lean_vocab = machine.states.clone();
    lean_vocab.sort();
    let mut rust_vocab: Vec<String> = ChildTerminal::ALL_KIND
        .iter()
        .map(|s| s.to_string())
        .collect();
    rust_vocab.sort();
    assert_eq!(
        lean_vocab, rust_vocab,
        "ChildTerminal vocabulary divergence between Lean and Rust"
    );

    for t in &machine.named_transitions {
        let expected = match t.from.as_str() {
            "interrupted" => "cancelled",
            "failed" | "dead" | "superseded" => "failed",
            other => panic!("unexpected ChildTerminal vocabulary: {}", other),
        };
        assert_eq!(
            t.to, expected,
            "Projection divergence at {}: Lean says target {}, Bucket 1 spec says {}",
            t.from, t.to, expected
        );
    }
    let mut sources_in_named: Vec<String> = machine
        .named_transitions
        .iter()
        .map(|t| t.from.clone())
        .collect();
    sources_in_named.sort();
    sources_in_named.dedup();
    assert_eq!(
        sources_in_named, rust_vocab,
        "ChildTerminal named_transitions must cover every ALL_KIND variant"
    );
}

#[test]
pub(super) fn lean_tool_call_cancel_actions_name_cancel_cause() {
    let machine = lean_state_machine_contract("ToolCall");
    let causes = lean_vocabulary_values("CancelCause");

    for cause in causes {
        let before = format!("cancelBeforeDispatch_{cause}");
        let during = format!("cancelDuringRun_{cause}");
        let held = format!("cancelWhileHeld_{cause}");
        assert!(
            machine.actions.iter().any(|action| action == &before),
            "ToolCall actions must include cause-qualified action {before:?}"
        );
        assert!(
            machine.actions.iter().any(|action| action == &during),
            "ToolCall actions must include cause-qualified action {during:?}"
        );
        assert!(
            machine.actions.iter().any(|action| action == &held),
            "ToolCall actions must include cause-qualified action {held:?}"
        );
    }

    assert!(
        !machine.actions.iter().any(|action| {
            action == "cancelBeforeDispatch"
                || action == "cancelDuringRun"
                || action == "cancelWhileHeld"
        }),
        "ToolCall cancel actions must name the CancelCause vocabulary string"
    );
}

#[test]
fn lean_emits_approval_actions_in_tool_call_machine() {
    let machine = lean_state_machine_contract("ToolCall");
    for name in [
        "holdForApproval",
        "recordApproval_approved",
        "recordApproval_denied",
        "approve",
        "deny",
        "timeoutWhileHeld",
    ] {
        assert!(
            machine.actions.iter().any(|action| action == name),
            "ToolCall actions must include approval action {name:?}"
        );
    }
}

#[test]
fn lean_emits_bridge_transitions_in_tool_call_machine() {
    let machine = lean_state_machine_contract("ToolCall");
    let bridge_names: Vec<&str> = vec![
        "background",
        "foreground",
        "detach_running",
        "detach_pending",
        "bridge_complete",
        "bridge_failure_failed",
        "bridge_failure_cancelled",
        "bridge_cancel_cascade",
    ];
    for name in &bridge_names {
        let found = machine.named_transitions.iter().any(|t| t.name == *name);
        assert!(
            found,
            "Lean contract must emit '{}' transition in ToolCall machine",
            name
        );
    }
}

#[test]
fn lean_marks_native_complete_fail_as_requires_native() {
    let machine = lean_state_machine_contract("ToolCall");
    let complete = machine
        .named_transitions
        .iter()
        .find(|t| t.name == "complete_native")
        .expect("ToolCall machine must have native complete transition (named complete_native)");
    assert!(
        complete.requires_native,
        "complete_native must be flagged with requires_native: true"
    );
    let fail = machine
        .named_transitions
        .iter()
        .find(|t| t.name == "fail_native")
        .expect("ToolCall machine must have native fail transition (named fail_native)");
    assert!(
        fail.requires_native,
        "fail_native must be flagged with requires_native: true"
    );
}
