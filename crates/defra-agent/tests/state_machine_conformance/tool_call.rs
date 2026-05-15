use super::*;

#[test]
fn tool_call_transitions_match_lean_contract() {
    // Spec-relational legal transitions
    assert_lean_transition_is_legal("ToolCall", "pending", "running");
    assert_lean_transition_is_legal("ToolCall", "pending", "failed");
    assert_lean_transition_is_legal("ToolCall", "pending", "cancelled");
    assert_lean_transition_is_legal("ToolCall", "running", "completed");
    assert_lean_transition_is_legal("ToolCall", "running", "failed");
    assert_lean_transition_is_legal("ToolCall", "running", "timedOut");
    assert_lean_transition_is_legal("ToolCall", "running", "cancelled");

    // T1 — terminal irreversibility
    assert_lean_transition_is_illegal("ToolCall", "completed", "running");
    assert_lean_transition_is_illegal("ToolCall", "failed", "running");
    assert_lean_transition_is_illegal("ToolCall", "timedOut", "running");
    assert_lean_transition_is_illegal("ToolCall", "cancelled", "running");
}

// ---------------------------------------------------------------------------
// R2 Bucket 2 — Lean transition matrix conformance for the subagent extensions.
//
// These tests assert that the Lean-emitted contract (consumed via
// `lean_state_machine_contract`) carries the new vocabularies (`AwaitMode`,
// `CancelPolicy`, `ChildTerminal`) and the new named transitions on the
// `ToolCall` machine that R2 introduced (mode flips, detach split, bridge_*
// edges, native-only `complete_native`/`fail_native` rows). Drift between
// Lean's model and Rust's runtime — for example, a vocabulary value added on
// only one side, or a Lean-only edge that Rust silently allows — is caught
// here rather than at PR review.
// ---------------------------------------------------------------------------

#[test]
pub(super) fn lean_emits_await_mode_vocabulary() {
    use defra_agent::tool_call_lifecycle::AwaitMode;

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
    use defra_agent::tool_call_lifecycle::CancelPolicy;

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
    use defra_agent::tool_call_lifecycle::ChildTerminal;

    let machine = lean_state_machine_contract("ChildTerminal");

    // Vocabulary check: Lean's source-side vocabulary must match Rust's
    // ChildTerminal::ALL_KIND.
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

    // Projection check: each named_transition's `from`/`to` must agree with
    // Lean's B2 projection rule (Subagent.lean): .interrupted -> .cancelled,
    // every other terminal -> .failed. Rust's `ChildTerminal::projected_state`
    // is verified to follow this rule by the Bucket 1 unit tests in
    // `tool_call_lifecycle.rs`; here we lock in that the *Lean* contract
    // emits exactly that table. (We can't call `projected_state` from this
    // integration test because its return type `ToolCallState` is
    // `pub(crate)` to defra-agent.)
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
    // Also assert every ChildTerminal variant has a corresponding row, so a
    // future Lean refactor that drops one is caught here.
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
