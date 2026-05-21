use crate::bridge::cause_derivation::{
    derive_response_cause, derive_tool_call_cause, RequestEvidence, ResponseEvidence,
    ToolCallEvidence,
};

fn req_default() -> RequestEvidence {
    RequestEvidence::default()
}
fn tool_default() -> ToolCallEvidence {
    ToolCallEvidence::default()
}

#[test]
fn user_cancelled_when_root_has_interrupt_and_no_parent_cascade() {
    let req = RequestEvidence {
        request_id: "req_root".into(),
        interrupt_requested_at: Some("2026-05-20T10:32:14Z".into()),
        caused_by_parent_request_id: None,
        deadline_breached: false,
    };
    let tool = ToolCallEvidence {
        tool_call_id: "tc_1".into(),
        lifecycle_state: Some("cancelled".into()),
        deadline_at: None,
        cancel_policy: Some("cascade".into()),
        completed_at: Some("2026-05-20T10:32:15Z".into()),
        timed_out: false,
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "userCancelled");
    assert_eq!(cause.source, "requestInterrupt");
    assert_eq!(cause.confidence, "direct");
    assert!(cause.evidence.iter().any(|e| e.contains("interrupt_requested_at")));
}

#[test]
fn interrupted_when_request_has_parent_cascade() {
    let req = RequestEvidence {
        request_id: "req_child".into(),
        interrupt_requested_at: None,
        caused_by_parent_request_id: Some("req_parent".into()),
        deadline_breached: false,
    };
    let tool = ToolCallEvidence {
        tool_call_id: "tc_2".into(),
        lifecycle_state: Some("cancelled".into()),
        cancel_policy: Some("cascade".into()),
        ..tool_default()
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "interrupted");
    assert_eq!(cause.source, "parentCascade");
    assert!(cause.evidence.iter().any(|e| e.contains("req_parent")));
}

#[test]
fn deadline_when_tool_lifecycle_is_timedout() {
    let tool = ToolCallEvidence {
        tool_call_id: "tc_3".into(),
        timed_out: true,
        lifecycle_state: Some("timedOut".into()),
        deadline_at: Some("2026-05-20T10:34:00Z".into()),
        completed_at: Some("2026-05-20T10:35:02Z".into()),
        cancel_policy: None,
    };
    let cause = derive_tool_call_cause(&req_default(), &tool).expect("derives");
    assert_eq!(cause.cause, "deadline");
    assert_eq!(cause.source, "toolLifecycle");
    assert!(cause.evidence.iter().any(|e| e.contains("timedOut")));
}

#[test]
fn deadline_wins_over_interrupted_when_both_signals_present() {
    // Per spec precedence rule 1: deadline check runs FIRST.
    let req = RequestEvidence {
        request_id: "req_child".into(),
        caused_by_parent_request_id: Some("req_parent".into()),
        ..req_default()
    };
    let tool = ToolCallEvidence {
        tool_call_id: "tc_4".into(),
        timed_out: true,
        lifecycle_state: Some("timedOut".into()),
        cancel_policy: Some("cascade".into()),
        ..tool_default()
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "deadline");
}

#[test]
fn interrupted_wins_over_user_cancelled_when_both_signals_present_on_child() {
    // Per spec precedence rule 2: parent cascade evidence wins over user-cancel
    // evidence on the *child*.
    let req = RequestEvidence {
        request_id: "req_child".into(),
        interrupt_requested_at: Some("2026-05-20T10:32:14Z".into()),
        caused_by_parent_request_id: Some("req_parent".into()),
        deadline_breached: false,
    };
    let tool = ToolCallEvidence {
        tool_call_id: "tc_5".into(),
        lifecycle_state: Some("cancelled".into()),
        cancel_policy: Some("cascade".into()),
        ..tool_default()
    };
    let cause = derive_tool_call_cause(&req, &tool).expect("derives");
    assert_eq!(cause.cause, "interrupted");
}

#[test]
fn unknown_when_cancelled_but_no_evidence() {
    let tool = ToolCallEvidence {
        tool_call_id: "tc_6".into(),
        lifecycle_state: Some("cancelled".into()),
        ..tool_default()
    };
    let cause = derive_tool_call_cause(&req_default(), &tool).expect("derives");
    assert_eq!(cause.cause, "unknown");
    assert_eq!(cause.source, "unresolved");
    // Evidence should enumerate what was checked.
    assert!(cause.evidence.iter().any(|e| e.contains("no parent cascade")));
    assert!(cause.evidence.iter().any(|e| e.contains("no deadline")));
    assert!(cause.evidence.iter().any(|e| e.contains("no interrupt_requested_at")));
}

#[test]
fn none_for_non_cancelled_tool_calls() {
    let tool = ToolCallEvidence {
        tool_call_id: "tc_7".into(),
        lifecycle_state: Some("completed".into()),
        ..tool_default()
    };
    assert!(derive_tool_call_cause(&req_default(), &tool).is_none());
}

#[test]
fn none_for_failed_tool_calls() {
    // "failed" is an error terminal — not a cancellation. Must return None.
    let tool = ToolCallEvidence {
        tool_call_id: "tc_8".into(),
        lifecycle_state: Some("failed".into()),
        ..tool_default()
    };
    assert!(
        derive_tool_call_cause(&req_default(), &tool).is_none(),
        "expected None for lifecycle_state=failed, but got Some(_)"
    );
}

#[test]
fn response_cause_uses_response_interrupted_at_when_present() {
    let resp = ResponseEvidence {
        interrupted_at: Some("2026-05-20T10:36:11Z".into()),
        completed_at: None,
    };
    let cause = derive_response_cause(&req_default(), &resp).expect("derives");
    assert_eq!(cause.cause, "interrupted");
    assert_eq!(cause.source, "responseInterruptedAt");
}

#[test]
fn response_cause_none_when_no_interrupted_at() {
    let resp = ResponseEvidence {
        interrupted_at: None,
        completed_at: Some("2026-05-20T10:36:11Z".into()),
    };
    assert!(derive_response_cause(&req_default(), &resp).is_none());
}
