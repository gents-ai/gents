use super::*;

fn request(origin: &str, retry_count: i64) -> AgentRequestRow {
    serde_json::from_value(serde_json::json!({
        "request_id": "request-1",
        "node_did": "did:test:node",
        "requester_did": "did:test:requester",
        "session_id": "session-1",
        "content": "try this",
        "lifecycle_state": "failed",
        "execution_origin": origin,
        "retry_count": retry_count
    }))
    .expect("request")
}

#[test]
fn projects_only_authoritatively_eligible_interactive_retry() {
    let interactive = request("interactive", 0);
    assert!(project_retry_eligibility(Some(&interactive)).eligible);

    let scheduled = request("scheduled", 0);
    let scheduled = project_retry_eligibility(Some(&scheduled));
    assert!(!scheduled.eligible);
    assert_eq!(
        scheduled.denial_reason.as_deref(),
        Some("nonInteractiveOrigin")
    );
}
