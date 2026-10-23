use gents::session_origin::SessionScope;

use crate::provenance::session_provenance;
use crate::tests::support::{create_request, create_tool_call, seed_provenance_fixture, OPERATOR};
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

    let metadata = view.related_sessions.as_ref().expect("related metadata");
    let mut related: Vec<_> = metadata
        .iter()
        .map(|summary| summary.session_id.as_str())
        .collect();
    related.sort_unstable();
    assert_eq!(related, ["sess_child", "sess_existing", "sess_peer"]);
    assert!(
        metadata
            .iter()
            .all(|summary| summary.message_count.is_none() && summary.tool_call_count.is_none()),
        "metadata reads must not hydrate transcript aggregates"
    );
    assert_eq!(view.session_id, "sess_parent");
    assert!(view.started_by.is_none() && view.received.is_empty());
    assert!(view.opening_request_id.is_none());
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
    assert_eq!(view.opening_request_id.as_deref(), Some("req_child"));
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
    let view = session_provenance(&core, scope("sess_child"))
        .await
        .expect("provenance");
    let sender = view
        .senders
        .iter()
        .find(|turn| turn.request_id == "req_child_2")
        .expect("canonical lineage attributes the replicated message");
    assert_eq!(
        sender.sender.cause_request_doc_id, parent,
        "the message remains attached to its exact physical causing request"
    );
    let parent_view = session_provenance(&core, scope("sess_parent"))
        .await
        .expect("parent provenance");
    let call = parent_view
        .calls
        .iter()
        .find(|call| call.tool_call_id == "tc_message")
        .expect("the causing agent-message call remains visible");
    assert_eq!(call.caused.request_id, "req_child_2");
    assert_eq!(sender.sender.session_id, "sess_parent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_session_message_call_lists_each_caused_request() {
    let (core, _tmp, parent) = seed_provenance_fixture().await;
    let call_doc = create_tool_call(&core, "tc_other", "run_workflow").await;
    for (request_id, session_id, created_at) in [
        ("req_other_a", "sess_other_a", "2026-05-20T00:05:00Z"),
        ("req_other_b", "sess_other_b", "2026-05-20T00:05:30Z"),
    ] {
        create_request(
            &core,
            request_id,
            OPERATOR,
            session_id,
            "processing",
            created_at,
            Some((&parent, "tc_other", &call_doc)),
        )
        .await;
    }
    let view = session_provenance(&core, scope("sess_parent"))
        .await
        .expect("provenance");

    let mut caused: Vec<_> = view
        .calls
        .iter()
        .filter(|call| call.tool_call_id == "tc_other")
        .map(|call| {
            (
                call.request_id.as_str(),
                call.caused.request_id.as_str(),
                call.caused.session_id.as_str(),
            )
        })
        .collect();
    caused.sort_unstable();
    assert_eq!(
        caused,
        [
            ("req_parent", "req_other_a", "sess_other_a"),
            ("req_parent", "req_other_b", "sess_other_b"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_message_calls_keep_one_view_each() {
    let (core, _tmp, parent) = seed_provenance_fixture().await;
    let call_doc = create_tool_call(&core, "tc_dup", "agent_new").await;
    for (request_id, session_id, created_at) in [
        ("req_dup_a", "sess_dup_a", "2026-05-20T00:07:00Z"),
        ("req_dup_b", "sess_dup_b", "2026-05-20T00:07:30Z"),
    ] {
        create_request(
            &core,
            request_id,
            OPERATOR,
            session_id,
            "processing",
            created_at,
            Some((&parent, "tc_dup", &call_doc)),
        )
        .await;
    }
    let view = session_provenance(&core, scope("sess_parent"))
        .await
        .expect("provenance");
    for call in ["tc_start", "tc_message", "tc_peer", "tc_existing"] {
        assert_eq!(
            view.calls
                .iter()
                .filter(|view| view.tool_call_id == call)
                .count(),
            1,
            "{call}"
        );
    }
    assert!(
        view.calls.iter().all(|view| view.tool_call_id != "tc_dup"),
        "a session-message call that caused several requests is ambiguous and shows none"
    );
}
