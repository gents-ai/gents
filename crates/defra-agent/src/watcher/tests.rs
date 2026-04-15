use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use super::cooldown::{take_next_eligible_pending_request, PROCESSED_REQUEST_COOLDOWN};
use super::*;

#[test]
fn agent_request_clone() {
    let req = AgentRequest {
        doc_id: "abc".into(),
        request_id: "req-1".into(),
        agent_did: "did:key:z123".into(),
        behavior_id: Some("general".into()),
        session_id: "sess-1".into(),
        content: "hello".into(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        created_at: "2026-03-12T00:00:00Z".into(),
    };
    let cloned = req.clone();
    assert_eq!(cloned.doc_id, "abc");
    assert_eq!(cloned.content, "hello");
}

#[test]
fn cooling_down_request_does_not_block_other_pending_sessions() {
    let now = Instant::now();
    let mut processed_request_ids = HashMap::from([("req-1".to_string(), now)]);

    let request = take_next_eligible_pending_request(
        &mut processed_request_ids,
        vec![request("req-1", "sess-1"), request("req-2", "sess-2")],
        now,
    )
    .expect("eligible request");

    assert_eq!(request.request_id, "req-2");
    assert!(processed_request_ids.contains_key("req-1"));
    assert!(processed_request_ids.contains_key("req-2"));
}

#[test]
fn cooled_down_request_becomes_eligible_again() {
    let now = Instant::now();
    let mut processed_request_ids = HashMap::from([("req-1".to_string(), now)]);
    let later = now + PROCESSED_REQUEST_COOLDOWN + Duration::from_millis(1);

    let request = take_next_eligible_pending_request(
        &mut processed_request_ids,
        vec![request("req-1", "sess-1")],
        later,
    )
    .expect("eligible request");

    assert_eq!(request.request_id, "req-1");
    assert_eq!(processed_request_ids.get("req-1").copied(), Some(later));
}

fn request(request_id: &str, session_id: &str) -> AgentRequest {
    AgentRequest {
        doc_id: format!("doc-{request_id}"),
        request_id: request_id.to_string(),
        agent_did: "did:key:z123".into(),
        behavior_id: Some("general".into()),
        session_id: session_id.to_string(),
        content: "hello".into(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        created_at: "2026-03-12T00:00:00Z".into(),
    }
}
