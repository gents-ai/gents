use gents_protocol::session::{AgentSession, SessionTitle, SessionTitleSource};

use super::verify_user_titled_session;

const NODE: &str = "did:test:fixture-node";
const AGENT: &str = "fixture-agent";
const SESSION: &str = "session-fixture";
const TITLE: &str = "Fixture session";

fn session(title: Option<SessionTitle>) -> AgentSession {
    AgentSession {
        session_id: SESSION.into(),
        node_did: NODE.into(),
        requester_did: Some(NODE.into()),
        agent_id: AGENT.into(),
        created_at: "2026-11-01T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title,
        tags: vec![],
        provenance: None,
        observation: None,
    }
}

fn user_title(text: &str) -> Option<SessionTitle> {
    Some(SessionTitle {
        text: text.into(),
        source: SessionTitleSource::User,
    })
}

fn verify(stored: &AgentSession) -> Result<(), String> {
    verify_user_titled_session(stored, NODE, AGENT, SESSION, TITLE)
}

#[test]
fn same_identity_with_the_expected_user_title_is_accepted() {
    assert_eq!(verify(&session(user_title(TITLE))), Ok(()));
}

#[test]
fn an_existing_placeholder_title_is_rejected() {
    // A placeholder is exactly the state in which the runtime still spawns
    // auxiliary title inference, so reusing the session would add a sibling
    // request the receipt does not list.
    let stored = session(Some(SessionTitle {
        text: TITLE.into(),
        source: SessionTitleSource::Placeholder,
    }));
    let mismatch = verify(&stored).expect_err("placeholder title must be rejected");
    assert!(mismatch.contains("Placeholder"), "{mismatch}");
}

#[test]
fn an_existing_generated_title_is_rejected() {
    let stored = session(Some(SessionTitle {
        text: TITLE.into(),
        source: SessionTitleSource::Generated,
    }));
    let mismatch = verify(&stored).expect_err("generated title must be rejected");
    assert!(mismatch.contains("Generated"), "{mismatch}");
}

#[test]
fn an_absent_title_is_rejected() {
    let mismatch = verify(&session(None)).expect_err("absent title must be rejected");
    assert!(mismatch.contains("absent"), "{mismatch}");
}

#[test]
fn a_different_user_title_text_is_rejected() {
    let stored = session(user_title("Some other session"));
    let mismatch = verify(&stored).expect_err("a different title must be rejected");
    assert!(mismatch.contains("Some other session"), "{mismatch}");
}

#[test]
fn an_empty_expected_title_is_rejected() {
    let stored = session(user_title(""));
    let mismatch = verify_user_titled_session(&stored, NODE, AGENT, SESSION, "")
        .expect_err("an empty expected title must be rejected");
    assert!(mismatch.contains("empty"), "{mismatch}");
}

#[test]
fn a_session_owned_by_another_node_is_rejected() {
    let mut stored = session(user_title(TITLE));
    stored.node_did = "did:test:other-node".into();
    let mismatch = verify(&stored).expect_err("a foreign node did must be rejected");
    assert!(mismatch.contains("node did"), "{mismatch}");
}

#[test]
fn a_session_with_another_requester_is_rejected() {
    let mut stored = session(user_title(TITLE));
    stored.requester_did = Some("did:test:other-requester".into());
    let mismatch = verify(&stored).expect_err("a foreign requester must be rejected");
    assert!(mismatch.contains("requester did"), "{mismatch}");
}

#[test]
fn a_session_without_a_requester_is_rejected() {
    let mut stored = session(user_title(TITLE));
    stored.requester_did = None;
    let mismatch = verify(&stored).expect_err("an absent requester must be rejected");
    assert!(mismatch.contains("requester did"), "{mismatch}");
}

#[test]
fn a_session_for_another_agent_is_rejected() {
    let mut stored = session(user_title(TITLE));
    stored.agent_id = "other-agent".into();
    let mismatch = verify(&stored).expect_err("a foreign agent must be rejected");
    assert!(mismatch.contains("agent id"), "{mismatch}");
}

#[test]
fn a_session_with_another_id_is_rejected() {
    let mut stored = session(user_title(TITLE));
    stored.session_id = "session-other".into();
    let mismatch = verify(&stored).expect_err("a foreign session id must be rejected");
    assert!(mismatch.contains("session id"), "{mismatch}");
}
