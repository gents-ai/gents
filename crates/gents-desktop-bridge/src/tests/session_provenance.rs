use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::session::SessionScope;
use serde_json::{json, Value};

use crate::provenance::{project_caused_calls, session_provenance};
use crate::tests::support::{provenance_fixture, ProvenanceFixture};
use crate::types::LinkedSessionView;

fn sessions(links: &[LinkedSessionView]) -> Vec<&str> {
    let mut ids: Vec<_> = links.iter().map(|link| link.session_id.as_str()).collect();
    ids.sort_unstable();
    ids
}

fn sorted(mut ids: Vec<&str>) -> Vec<&str> {
    ids.sort_unstable();
    ids
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maps_the_lineage_owner_and_each_call_to_its_caused_request() {
    let fixture = provenance_fixture().await;
    let ProvenanceFixture {
        desktop,
        parent_doc,
        child_session,
        child_opening,
        child_message,
        existing_message,
        ..
    } = &fixture;
    let view = session_provenance(&desktop.core, fixture.parent())
        .await
        .expect("provenance");

    let metadata = view.related_sessions.as_ref().expect("related metadata");
    let mut related: Vec<_> = metadata
        .iter()
        .map(|summary| summary.session_id.as_str())
        .collect();
    related.sort_unstable();
    assert_eq!(
        related,
        sorted(vec![child_session.as_str(), "sess_existing"])
    );
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
        [child_session.as_str()],
        "started sessions are the sessions whose stored provenance names this session"
    );
    assert_eq!(
        sessions(&view.sent),
        ["sess_existing"],
        "a message into a session the Agent opened as a client is not a start"
    );
    assert!(view
        .started
        .iter()
        .all(|link| &link.cause_request_doc_id == parent_doc));

    let mut calls: Vec<_> = view
        .calls
        .iter()
        .map(|call| {
            (
                call.tool_call_id.as_str(),
                call.request_id.as_str(),
                call.caused.request_id.as_str(),
                call.caused.session_id.as_str(),
                call.caused
                    .lifecycle_state
                    .map(RequestLifecycleState::as_str),
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
                existing_message.as_str(),
                "sess_existing",
                Some("processing")
            ),
            (
                "tc_message",
                "req_parent",
                child_message.as_str(),
                child_session.as_str(),
                Some("processing")
            ),
            (
                "tc_start",
                "req_parent",
                child_opening.as_str(),
                child_session.as_str(),
                Some("completed")
            ),
        ]
    );
    assert!(view
        .calls
        .iter()
        .all(|call| call.caused.node_did == desktop.node_did()));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_started_session_names_its_starter_and_the_sender_of_each_turn() {
    let fixture = provenance_fixture().await;
    let view = session_provenance(&fixture.desktop.core, fixture.child())
        .await
        .expect("provenance");

    assert_eq!(
        view.started_by
            .as_ref()
            .map(|link| link.session_id.as_str()),
        Some("sess_parent")
    );
    assert!(view.started.is_empty() && view.sent.is_empty() && view.calls.is_empty());
    assert_eq!(
        view.opening_request_id.as_deref(),
        Some(fixture.child_opening.as_str())
    );
    let mut senders: Vec<_> = view
        .senders
        .iter()
        .map(|turn| (turn.request_id.as_str(), turn.sender.session_id.as_str()))
        .collect();
    senders.sort_unstable();
    let mut expected = vec![
        (fixture.child_opening.as_str(), "sess_parent"),
        (fixture.child_message.as_str(), "sess_parent"),
    ];
    expected.sort_unstable();
    assert_eq!(senders, expected);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_messaged_session_was_not_started_by_its_sender() {
    let fixture = provenance_fixture().await;
    let view = session_provenance(&fixture.desktop.core, fixture.existing())
        .await
        .expect("provenance");
    assert!(view.started_by.is_none());
    assert_eq!(sessions(&view.received), ["sess_parent"]);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_root_session_has_no_provenance() {
    let fixture = provenance_fixture().await;
    let view = session_provenance(&fixture.desktop.core, fixture.unrelated())
        .await
        .expect("provenance");
    assert!(view.started_by.is_none());
    assert!(view.received.is_empty() && view.sent.is_empty() && view.started.is_empty());
    assert!(view.calls.is_empty() && view.senders.is_empty());
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_message_keeps_the_call_that_sent_it() {
    let fixture = provenance_fixture().await;
    let view = session_provenance(&fixture.desktop.core, fixture.child())
        .await
        .expect("provenance");
    let sender = view
        .senders
        .iter()
        .find(|turn| turn.request_id == fixture.child_message)
        .expect("canonical lineage attributes the message");
    assert_eq!(
        sender.sender.cause_request_doc_id, fixture.parent_doc,
        "the message remains attached to its exact physical causing request"
    );
    let parent_view = session_provenance(&fixture.desktop.core, fixture.parent())
        .await
        .expect("parent provenance");
    let call = parent_view
        .calls
        .iter()
        .find(|call| call.tool_call_id == "tc_message")
        .expect("the causing agent-message call remains visible");
    assert_eq!(call.caused.request_id, fixture.child_message);
    assert_eq!(sender.sender.session_id, "sess_parent");
    fixture.shutdown().await;
}

/// A caused request row as the lineage owner reads it.
fn caused(doc: &str, request_id: &str, session_id: &str, created_at: &str, call: &str) -> Value {
    json!({
        "_docID": doc, "node_did": "did:test:node", "session_id": session_id,
        "requester_did": "did:test:lead", "request_id": request_id, "created_at": created_at,
        "lifecycle_state": "processing", "caused_by_parent_tool_call_doc_id": call,
    })
}

fn call(doc: &str, tool_call_id: &str, tool_name: &str) -> Value {
    json!({
        "_docID": doc, "request_id": "req_parent", "tool_call_id": tool_call_id,
        "tool_name": tool_name,
    })
}

#[test]
fn a_non_session_message_call_lists_each_caused_request() {
    let states = [
        caused(
            "doc_other_a",
            "req_other_a",
            "sess_other_a",
            "2026-05-20T00:05:00Z",
            "call_other",
        ),
        caused(
            "doc_other_b",
            "req_other_b",
            "sess_other_b",
            "2026-05-20T00:05:30Z",
            "call_other",
        ),
    ];
    let calls = [call("call_other", "tc_other", "run_workflow")];
    let view = project_caused_calls(&states, &calls).unwrap();

    let mut caused: Vec<_> = view
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

#[test]
fn session_message_calls_keep_one_view_each() {
    let states = [
        caused(
            "doc_child",
            "req_child",
            "sess_child",
            "2026-05-20T00:01:00Z",
            "call_start",
        ),
        caused(
            "doc_child_2",
            "req_child_2",
            "sess_child",
            "2026-05-20T00:02:00Z",
            "call_message",
        ),
        caused(
            "doc_existing_2",
            "req_existing_2",
            "sess_existing",
            "2026-05-20T00:04:00Z",
            "call_existing",
        ),
        caused(
            "doc_dup_a",
            "req_dup_a",
            "sess_dup_a",
            "2026-05-20T00:07:00Z",
            "call_dup",
        ),
        caused(
            "doc_dup_b",
            "req_dup_b",
            "sess_dup_b",
            "2026-05-20T00:07:30Z",
            "call_dup",
        ),
    ];
    let calls = [
        call("call_start", "tc_start", "agent_new"),
        call("call_message", "tc_message", "agent_message"),
        call("call_existing", "tc_existing", "agent_message"),
        call("call_dup", "tc_dup", "agent_new"),
    ];
    let view = project_caused_calls(&states, &calls).unwrap();
    for call in ["tc_start", "tc_message", "tc_existing"] {
        assert_eq!(
            view.iter().filter(|view| view.tool_call_id == call).count(),
            1,
            "{call}"
        );
    }
    assert!(
        view.iter().all(|view| view.tool_call_id != "tc_dup"),
        "a session-message call that caused several requests is ambiguous and shows none"
    );
}

impl ProvenanceFixture {
    fn scope(&self, session_id: &str, requester_did: &str) -> SessionScope {
        SessionScope {
            node_did: self.desktop.node_did().into(),
            session_id: session_id.into(),
            requester_did: Some(requester_did.into()),
        }
    }

    fn parent(&self) -> SessionScope {
        self.scope("sess_parent", &self.desktop.user_did())
    }

    fn unrelated(&self) -> SessionScope {
        self.scope("sess_unrelated", &self.desktop.user_did())
    }

    fn child(&self) -> SessionScope {
        self.scope(&self.child_session, &self.desktop.lead.agent_did)
    }

    fn existing(&self) -> SessionScope {
        self.scope("sess_existing", &self.desktop.lead.agent_did)
    }
}
