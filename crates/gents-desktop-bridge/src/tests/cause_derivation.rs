use crate::cause_derivation::{
    derive_request_cause, derive_tool_call_cause, RequestEvidence, ToolCallEvidence,
};

fn req_default() -> RequestEvidence {
    RequestEvidence::default()
}
fn tool_default() -> ToolCallEvidence {
    ToolCallEvidence::default()
}

#[test]
fn user_cancelled_when_request_has_interrupt_latch() {
    let req = RequestEvidence {
        interrupt_requested_at: Some("2026-05-20T10:32:14Z".into()),
    };
    let tool = ToolCallEvidence {
        lifecycle_state: Some("cancelled".into()),
        deadline_at: None,
        completed_at: Some("2026-05-20T10:32:15Z".into()),
        timed_out: false,
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "userCancelled");
    assert_eq!(cause.source, "requestInterrupt");
    assert_eq!(cause.confidence, "direct");
    assert!(cause
        .evidence
        .iter()
        .any(|e| e.contains("interrupt_requested_at")));
}

#[test]
fn deadline_when_tool_lifecycle_is_timedout() {
    let tool = ToolCallEvidence {
        timed_out: true,
        lifecycle_state: Some("timedOut".into()),
        deadline_at: Some("2026-05-20T10:34:00Z".into()),
        completed_at: Some("2026-05-20T10:35:02Z".into()),
    };
    let cause = derive_tool_call_cause(&req_default(), &tool).expect("derives");
    assert_eq!(cause.cause, "deadline");
    assert_eq!(cause.source, "toolLifecycle");
    assert!(cause.evidence.iter().any(|e| e.contains("timedOut")));
}

#[test]
fn deadline_wins_over_interrupt_latch_when_both_signals_present() {
    let req = RequestEvidence {
        interrupt_requested_at: Some("2026-05-20T10:32:14Z".into()),
    };
    let tool = ToolCallEvidence {
        timed_out: true,
        lifecycle_state: Some("timedOut".into()),
        ..tool_default()
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "deadline");
}

#[test]
fn unknown_when_cancelled_but_no_evidence() {
    let tool = ToolCallEvidence {
        lifecycle_state: Some("cancelled".into()),
        ..tool_default()
    };
    let cause = derive_tool_call_cause(&req_default(), &tool).expect("derives");
    assert_eq!(cause.cause, "unknown");
    assert_eq!(cause.source, "unresolved");
    assert!(cause.evidence.iter().any(|e| e.contains("no deadline")));
    assert!(cause
        .evidence
        .iter()
        .any(|e| e.contains("no interrupt_requested_at")));
}

#[test]
fn none_for_non_cancelled_tool_calls() {
    let tool = ToolCallEvidence {
        lifecycle_state: Some("completed".into()),
        ..tool_default()
    };
    assert!(derive_tool_call_cause(&req_default(), &tool).is_none());
}

#[test]
fn none_for_failed_tool_calls() {
    let tool = ToolCallEvidence {
        lifecycle_state: Some("failed".into()),
        ..tool_default()
    };
    assert!(
        derive_tool_call_cause(&req_default(), &tool).is_none(),
        "expected None for lifecycle_state=failed, but got Some(_)"
    );
}

#[test]
fn request_cause_uses_interrupted_lifecycle_and_terminal_timestamp() {
    let cause = derive_request_cause(
        Some("interrupted"),
        &req_default(),
        Some("2026-05-20T10:36:11Z".into()),
    )
    .expect("derives");
    assert_eq!(cause.cause, "interrupted");
    assert_eq!(cause.source, "requestLifecycle");
    assert_eq!(cause.at.as_deref(), Some("2026-05-20T10:36:11Z"));
}

#[test]
fn request_cause_none_without_cancellation_evidence() {
    for state in [
        None,
        Some("pending"),
        Some("processing"),
        Some("completed"),
        Some("failed"),
    ] {
        assert!(derive_request_cause(state, &req_default(), None).is_none());
    }
}
