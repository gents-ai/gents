use super::target::session_target_allowed;
use super::*;

use gents_protocol::tool_call::AwaitMode;
use gents_store::identity::NodeIdentity as _;

/// The caller-visible session and grants one Lean session-target case describes,
/// as `caller` on node `caller`.
fn session_target_fixture(
    same_requester: bool,
    same_node: bool,
    caused: bool,
    allowed_target: bool,
) -> (gents_protocol::session::AgentSession, AgentToolConfig) {
    use gents_protocol::session::{AgentSession, SessionProvenance};
    let session = AgentSession {
        session_id: "worker-session".to_owned(),
        node_did: if same_node { "caller" } else { "peer" }.to_owned(),
        requester_did: Some(if same_requester { "caller" } else { "other" }.to_owned()),
        agent_did: "worker".to_owned(),
        created_at: "2026-10-14T00:00:00Z".to_owned(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        observation: None,
        provenance: caused.then(|| SessionProvenance {
            parent_request_doc_id: Some("parent".to_owned()),
            ..Default::default()
        }),
    };
    let tools = AgentToolConfig {
        enabled: true,
        targets: if allowed_target {
            vec![gents_documents::AgentTargetDocument {
                target_id: "worker".to_owned(),
                node_did: "caller".to_owned(),
                target_node_did: session.node_did.clone(),
                agent_did: "worker".to_owned(),
                name: "worker".to_owned(),
                description: None,
                tags: Vec::new(),
            }]
        } else {
            Vec::new()
        },
    };
    (session, tools)
}

#[test]
fn session_target_authority_matches_lean_owner() {
    let cases = &gents_lean_vocab::lean_causal_hop_contract().session_target_cases;
    assert_eq!(cases.len(), 16);
    for case in cases {
        let (session, tools) = session_target_fixture(
            case.same_requester,
            case.same_node,
            case.caused,
            case.allowed_target,
        );
        assert_eq!(
            session_target_allowed(&session, "caller", "caller", &tools),
            case.expected_allowed,
            "{case:?}"
        );
    }
}

#[test]
fn session_list_entries_match_lean_owner() {
    let cases = &gents_lean_vocab::lean_causal_hop_contract().session_list_cases;
    assert_eq!(cases.len(), 64);
    for case in cases {
        let (session, tools) = session_target_fixture(
            case.same_requester,
            case.same_node,
            case.caused,
            case.allowed_target,
        );
        let entry = super::discovery::listed_session(
            session_target_allowed(&session, "caller", "caller", &tools),
            case.verified_starter_receipt,
            case.interrupt_allowed,
        );
        assert_eq!(entry.is_some(), case.expected_listed, "{case:?}");
        assert_eq!(
            entry.is_some_and(|entry| entry.can_message),
            case.expected_can_message,
            "{case:?}"
        );
        assert_eq!(
            entry.is_some_and(|entry| entry.can_interrupt),
            case.expected_can_interrupt,
            "{case:?}"
        );
    }
}
