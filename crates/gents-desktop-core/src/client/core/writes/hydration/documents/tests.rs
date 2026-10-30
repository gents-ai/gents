use super::*;
use serde_json::json;

#[test]
fn hydration_start_evidence_rejects_unknown_request_lifecycle_state() {
    let response = defra_node::QueryResponse::success(json!({
        "AgentSession": [{ "_docID": "session-1" }],
        "AgentRequest": [{
            "request_id": "request-1",
            "lifecycle_state": "notARequestState"
        }]
    }));

    let error = local_hydration_start_evidence_from_response(&response)
        .expect_err("unknown lifecycle state must fail at the row boundary");
    assert!(
        format!("{error:#}").contains("notARequestState"),
        "{error:#}"
    );
}

#[test]
fn hydration_query_escapes_every_scope_value() {
    let query = local_hydration_query(
        "did:key:requester\"escaped",
        "session\"escaped",
        "did:key:node\"escaped",
        None,
    );
    assert!(query.contains(r#"requester_did: {_eq: "did:key:requester\"escaped"}"#));
    assert!(query.contains(r#"session_id: { _eq: "session\"escaped" }"#));
    assert!(query.contains(r#"node_did: {_eq: "did:key:node\"escaped"}"#));
}
