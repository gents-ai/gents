use gents::session_origin::SessionScope;

use crate::provenance::session_provenance;
use crate::tests::support::{seed_provenance_fixture, OPERATOR};
use crate::types::LinkedSessionView;

fn scope(session_id: &str) -> SessionScope {
    SessionScope {
        node_did: OPERATOR.into(),
        session_id: session_id.into(),
        requester_did: Some(OPERATOR.into()),
    }
}

fn sessions(links: &[LinkedSessionView]) -> Vec<&str> {
    let mut ids: Vec<_> = links.iter().map(|link| link.session_id.as_str()).collect();
    ids.sort_unstable();
    ids
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maps_the_lineage_owner_and_each_call_to_its_caused_request() {
    let (core, _tmp, parent_doc_id) = seed_provenance_fixture().await;
    let view = session_provenance(&core, scope("sess_parent"))
        .await
        .expect("provenance");

    assert_eq!(view.session_id, "sess_parent");
    assert!(view.started_by.is_none() && view.received.is_empty());
    assert_eq!(
        sessions(&view.started),
        ["sess_child", "sess_peer"],
        "started sessions are the sessions whose stored provenance names this session"
    );
    assert_eq!(
        sessions(&view.sent),
        ["sess_existing"],
        "a message into a session the person started is not a start"
    );
    assert!(view
        .started
        .iter()
        .all(|link| link.cause_request_doc_id == parent_doc_id));

    let mut calls: Vec<_> = view
        .calls
        .iter()
        .map(|call| {
            (
                call.tool_call_id.as_str(),
                call.request_id.as_str(),
                call.caused.request_id.as_str(),
                call.caused.session_id.as_str(),
                call.caused.lifecycle_state.as_deref(),
            )
        })
        .collect();
    calls.sort_unstable();
    assert_eq!(
        calls,
        [
            (
                "tc_existing",
                "req_parent",
                "req_existing_2",
                "sess_existing",
                Some("processing")
            ),
            (
                "tc_message",
                "req_parent",
                "req_child_2",
                "sess_child",
                Some("processing")
            ),
            (
                "tc_peer",
                "req_parent",
                "req_peer",
                "sess_peer",
                Some("processing")
            ),
            (
                "tc_start",
                "req_parent",
                "req_child",
                "sess_child",
                Some("completed")
            ),
        ]
    );
    let peer = view
        .calls
        .iter()
        .find(|call| call.tool_call_id == "tc_peer")
        .unwrap();
    assert_eq!(peer.caused.node_did, "did:test:other");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_started_session_names_its_starter_and_the_sender_of_each_turn() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    for request_id in ["req_parent", "req_child", "req_child_2"] {
        core.refresh_local_request(OPERATOR, request_id)
            .await
            .expect("refresh request");
    }
    let view = session_provenance(&core, scope("sess_child"))
        .await
        .expect("provenance");

    assert_eq!(
        view.started_by
            .as_ref()
            .map(|link| link.session_id.as_str()),
        Some("sess_parent")
    );
    assert!(view.started.is_empty() && view.sent.is_empty() && view.calls.is_empty());
    let mut senders: Vec<_> = view
        .senders
        .iter()
        .map(|turn| (turn.request_id.as_str(), turn.sender.session_id.as_str()))
        .collect();
    senders.sort_unstable();
    assert_eq!(
        senders,
        [("req_child", "sess_parent"), ("req_child_2", "sess_parent")]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_messaged_session_was_not_started_by_its_sender() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    let view = session_provenance(&core, scope("sess_existing"))
        .await
        .expect("provenance");
    assert!(view.started_by.is_none());
    assert_eq!(sessions(&view.received), ["sess_parent"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_root_session_has_no_provenance() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    let view = session_provenance(&core, scope("sess_unrelated"))
        .await
        .expect("provenance");
    assert!(view.started_by.is_none());
    assert!(view.received.is_empty() && view.sent.is_empty() && view.started.is_empty());
    assert!(view.calls.is_empty() && view.senders.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replicated_session_message_keeps_the_call_that_sent_it() {
    let (core, _tmp, parent) = seed_provenance_fixture().await;
    core.refresh_local_request(OPERATOR, "req_child_2")
        .await
        .expect("refresh request");
    let store = core.store().snapshot();
    let row = store
        .request_row("req_child_2")
        .expect("replicated request");
    assert!(row.caused_by_parent_tool_call_doc_id.is_some());
    assert_eq!(
        gents::lifecycle::request_origin(row),
        gents::lifecycle::RequestOrigin::SessionMessage {
            parent_request_doc_id: Some(parent.as_str())
        }
    );
}
