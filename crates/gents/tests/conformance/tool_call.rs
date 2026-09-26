//! Vocabulary comparisons against production owners. Lifecycle transitions
//! and process termination are exercised by their runtime tests,
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

/// AwaitMode stays a vocabulary (not a state machine) and matches the Rust
/// tool-call owner.
#[test]
fn lean_emits_await_mode_vocabulary() {
    use gents::tool_call_lifecycle::AwaitMode;

    assert_eq!(
        lean_vocabulary_values("AwaitMode"),
        AwaitMode::ALL
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>(),
        "AwaitMode vocabulary divergence between Lean and Rust"
    );
}
