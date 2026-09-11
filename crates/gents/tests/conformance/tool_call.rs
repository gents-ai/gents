//! Vocabulary comparisons against production owners. Lifecycle transitions,
//! bridge writes and process termination are exercised by their runtime tests,
//! not by checking names in the emitted Lean machine.
use super::*;

#[test]
fn cancel_cause_vocabulary_matches_production() {
    use gents::tool_call_lifecycle::CancelCause;
    assert_eq!(
        lean_vocabulary_values("CancelCause"),
        CancelCause::ALL
            .iter()
            .map(|cause| cause.as_str())
            .collect::<Vec<_>>()
    );
}

/// AwaitMode and CancelPolicy stay vocabularies (not state machines) and match
/// the Rust bridge-configuration owners.
#[test]
fn lean_emits_await_mode_and_cancel_policy_vocabularies() {
    use gents::tool_call_lifecycle::{AwaitMode, CancelPolicy};

    let await_modes = lean_vocabulary_values("AwaitMode");
    assert_eq!(
        await_modes,
        AwaitMode::ALL
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>(),
        "AwaitMode vocabulary divergence between Lean and Rust"
    );

    let cancel_policies = lean_vocabulary_values("CancelPolicy");
    assert_eq!(
        cancel_policies,
        CancelPolicy::ALL
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        "CancelPolicy vocabulary divergence between Lean and Rust"
    );
}

#[test]
fn lean_emits_child_terminal_vocabulary() {
    assert_eq!(
        lean_vocabulary_values("ChildTerminal"),
        gents::tool_call_lifecycle::ChildTerminal::ALL_KIND,
        "ChildTerminal vocabulary divergence between Lean and Rust"
    );
}
