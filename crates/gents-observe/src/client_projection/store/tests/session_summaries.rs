use super::super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::session::{SessionObservation, SessionRequestObservation};

fn session(
    session_id: &str,
    node_did: &str,
    created_at: &str,
    active: Option<&str>,
) -> AgentSession {
    let mut session: AgentSession = serde_json::from_value(serde_json::json!({
        "session_id": session_id, "node_did": node_did, "agent_did": "agent",
        "created_at": created_at,
    }))
    .unwrap();
    session.observation = active.map(|at| SessionObservation {
        reply_preview: None,
        last_activity_at: at.into(),
        preview: None,
        latest_request: Some(SessionRequestObservation {
            request_doc_id: format!("{session_id}-physical"),
            request_id: format!("{session_id}-logical"),
            lifecycle_state: RequestLifecycleState::Completed,
        }),
    });
    session
}

#[test]
fn entries_order_by_activity_then_creation_and_bind_the_exact_indexed_request() {
    let sessions = [
        session("idle", "node", "2026-09-05T00:00:00Z", None),
        session(
            "older",
            "node",
            "2026-09-01T00:00:00Z",
            Some("2026-09-02T00:00:00Z"),
        ),
        session(
            "newer",
            "node",
            "2026-09-01T00:00:00Z",
            Some("2026-09-03T00:00:00Z"),
        ),
        session(
            "foreign",
            "other",
            "2026-09-09T00:00:00Z",
            Some("2026-09-09T00:00:00Z"),
        ),
    ];
    let running = AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("newer-physical".into()),
        request_id: "newer-logical".into(),
        node_did: Some("node".into()),
        session_id: Some("newer".into()),
        agent_did: Some("agent".into()),
        lifecycle_state: Some(RequestLifecycleState::Processing),
        ..Default::default()
    };
    let entries = session_summary_entries(&sessions, std::slice::from_ref(&running), "node");
    let order = entries
        .iter()
        .map(|entry| entry.session.session_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(order, ["newer", "older", "idle"]);
    let newer = &entries[0];
    assert_eq!(
        newer.request.and_then(|row| row.doc_id.as_deref()),
        Some("newer-physical")
    );
    assert_eq!(newer.lifecycle, Some(RequestLifecycleState::Processing));
    assert_eq!(newer.turn_state, Some(ClientTurnState::Running));
    let older = &entries[1];
    assert!(older.request.is_none());
    assert_eq!(older.lifecycle, Some(RequestLifecycleState::Completed));
    assert_eq!(older.latest_request_id, Some("older-logical"));
    assert!(entries[2].latest_request_doc_id.is_none());
}
