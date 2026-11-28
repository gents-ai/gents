use gents_protocol::request_lifecycle::RequestLifecycleState;

use crate::cause_derivation::{
    derive_request_cause, derive_tool_call_cause, RequestEvidence, ToolCallEvidence,
};

#[test]
fn tool_call_cause_derivation_follows_priority_and_ignores_non_cancelled() {
    let interrupted_req = RequestEvidence {
        interrupted_at: Some("2026-05-20T10:32:14Z".into()),
    };
    let cancelled = ToolCallEvidence {
        lifecycle_state: Some("cancelled".into()),
        completed_at: Some("2026-05-20T10:32:15Z".into()),
        ..Default::default()
    };
    let cause = derive_tool_call_cause(&interrupted_req, &cancelled).expect("derives");
    assert_eq!(
        (
            cause.cause.as_str(),
            cause.source.as_str(),
            cause.confidence.as_str(),
            cause.at.as_deref()
        ),
        (
            "interrupted",
            "requestInterrupt",
            "direct",
            Some("2026-05-20T10:32:14Z")
        )
    );

    let timed_out = ToolCallEvidence {
        timed_out: true,
        lifecycle_state: Some("timedOut".into()),
        deadline_at: Some("2026-05-20T10:34:00Z".into()),
        completed_at: Some("2026-05-20T10:35:02Z".into()),
    };
    let deadline =
        derive_tool_call_cause(&RequestEvidence::default(), &timed_out).expect("derives");
    assert_eq!(
        (deadline.cause.as_str(), deadline.source.as_str()),
        ("deadline", "toolLifecycle")
    );
    assert!(deadline.evidence.iter().any(|e| e.contains("timedOut")));

    let both = derive_tool_call_cause(&interrupted_req, &timed_out).expect("derives");
    assert_eq!(both.cause, "deadline");

    let unknown = derive_tool_call_cause(&RequestEvidence::default(), &cancelled).expect("derives");
    assert_eq!(
        (unknown.cause.as_str(), unknown.source.as_str()),
        ("unknown", "unresolved")
    );
    assert!(unknown.evidence.iter().any(|e| e.contains("no deadline")));
    assert!(unknown
        .evidence
        .iter()
        .any(|e| e.contains("root request not interrupted")));

    for state in ["completed", "failed"] {
        let tool = ToolCallEvidence {
            lifecycle_state: Some(state.into()),
            ..Default::default()
        };
        assert!(
            derive_tool_call_cause(&RequestEvidence::default(), &tool).is_none(),
            "expected None for lifecycle_state={state}"
        );
    }
}

#[test]
fn request_cause_derivation_requires_interrupted_lifecycle() {
    let cause = derive_request_cause(
        Some(RequestLifecycleState::Interrupted),
        Some("2026-05-20T10:36:11Z".into()),
    )
    .expect("derives");
    assert_eq!(
        (
            cause.cause.as_str(),
            cause.source.as_str(),
            cause.at.as_deref()
        ),
        (
            "interrupted",
            "requestLifecycle",
            Some("2026-05-20T10:36:11Z")
        )
    );
    for state in [
        None,
        Some(RequestLifecycleState::Pending),
        Some(RequestLifecycleState::Processing),
        Some(RequestLifecycleState::Completed),
        Some(RequestLifecycleState::Failed),
    ] {
        assert!(derive_request_cause(state, None).is_none());
    }
}
