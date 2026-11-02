use super::*;

#[test]
fn cancel_cause_vocabulary_matches_production() {
    use gents::tools::call::lifecycle::CancelCause;
    assert_eq!(
        lean_vocabulary_values("CancelCause"),
        CancelCause::ALL
            .iter()
            .map(|cause| cause.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn lean_emits_await_mode_vocabulary() {
    use gents::tools::call::lifecycle::AwaitMode;

    assert_eq!(
        lean_vocabulary_values("AwaitMode"),
        AwaitMode::ALL
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>(),
        "AwaitMode vocabulary divergence between Lean and Rust"
    );
}
