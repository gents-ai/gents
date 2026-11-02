use super::{session_current_hop, SessionRequestFact};

fn fact(request_id: &str, created_at: &str, hop: u32) -> SessionRequestFact {
    SessionRequestFact {
        node_did: "did:test:agent".into(),
        session_id: "session".into(),
        requester_did: None,
        agent_id: "general".into(),
        created_at: created_at.into(),
        hop,
        observed: gents_protocol::session::SessionRequestObservation {
            request_doc_id: format!("doc-{request_id}"),
            request_id: request_id.into(),
            lifecycle_state: gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
        },
    }
}

/// Lean `CausalHop.same_second_tie_takes_the_highest_hop`: a refused
/// over-bound wake and a native wake written in the same second read as
/// the refused hop, whatever their request ids.
#[test]
fn a_same_second_tie_takes_the_highest_hop() {
    let facts = [
        fact("parent", "2026-09-27T05:12:04Z", 0),
        fact("a-refused-wake", "2026-09-27T05:12:05Z", 9),
        fact("z-native-wake", "2026-09-27T05:12:05Z", 3),
    ];
    assert_eq!(session_current_hop(&facts), 9);
    let reversed = [
        fact("parent", "2026-09-27T05:12:04Z", 0),
        fact("z-refused-wake", "2026-09-27T05:12:05Z", 9),
        fact("a-native-wake", "2026-09-27T05:12:05Z", 3),
    ];
    assert_eq!(session_current_hop(&reversed), 9);
    // An earlier second never counts, however high its hop.
    let later_root = [
        fact("refused-wake", "2026-09-27T05:12:04Z", 9),
        fact("user-root", "2026-09-27T05:12:05Z", 0),
    ];
    assert_eq!(session_current_hop(&later_root), 0);
    assert_eq!(session_current_hop(&[]), 0);
}
