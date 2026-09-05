use super::*;

fn request(origin: &str, retry_count: i64, max_retries: i64) -> AgentRequestRow {
    serde_json::from_value(serde_json::json!({
        "request_id": "request-1",
        "agent_did": "did:test:agent",
        "requester_did": "did:test:requester",
        "session_id": "session-1",
        "content": "try this",
        "lifecycle_state": "failed",
        "execution_origin": origin,
        "retry_count": retry_count,
        "max_retries": max_retries
    }))
    .expect("request")
}

#[test]
fn projects_only_authoritatively_eligible_interactive_retry() {
    let interactive = request("interactive", 0, 3);
    assert!(project_retry_eligibility(Some(&interactive)).eligible);

    let scheduled = request("scheduled", 0, 3);
    let scheduled = project_retry_eligibility(Some(&scheduled));
    assert!(!scheduled.eligible);
    assert_eq!(
        scheduled.denial_reason.as_deref(),
        Some("nonInteractiveOrigin")
    );

    let exhausted = request("interactive", 3, 3);
    let exhausted = project_retry_eligibility(Some(&exhausted));
    assert!(!exhausted.eligible);
    assert_eq!(
        exhausted.denial_reason.as_deref(),
        Some("retryBudgetExhausted")
    );
}
