use std::sync::Arc;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::session::SessionScope;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::host::{protocol, ClientHost};
use crate::provenance::{project_caused_calls, session_provenance};
use crate::tests::support::{attach_host, call_host, provenance_fixture, ProvenanceFixture};
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
async fn session_provenance_tracks_lineage_senders_and_host_wire_reads() {
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

    let parent_view = session_provenance(&desktop.core, fixture.parent())
        .await
        .expect("provenance");
    let metadata = parent_view
        .related_sessions
        .as_ref()
        .expect("related metadata");
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
    assert_eq!(parent_view.session_id, "sess_parent");
    assert!(parent_view.started_by.is_none() && parent_view.received.is_empty());
    assert!(parent_view.opening_request_id.is_none());
    assert_eq!(
        sessions(&parent_view.started),
        [child_session.as_str()],
        "started sessions are the sessions whose stored provenance names this session"
    );
    assert_eq!(
        sessions(&parent_view.sent),
        ["sess_existing"],
        "a message into a session the Agent opened as a client is not a start"
    );
    assert!(parent_view
        .started
        .iter()
        .all(|link| &link.cause_request_doc_id == parent_doc));

    let mut calls: Vec<_> = parent_view
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
    assert!(parent_view
        .calls
        .iter()
        .all(|call| call.caused.node_did == desktop.node_did()));

    let child_view = session_provenance(&desktop.core, fixture.child())
        .await
        .expect("child provenance");
    assert_eq!(
        child_view
            .started_by
            .as_ref()
            .map(|link| link.session_id.as_str()),
        Some("sess_parent")
    );
    assert!(
        child_view.started.is_empty() && child_view.sent.is_empty() && child_view.calls.is_empty()
    );
    assert_eq!(
        child_view.opening_request_id.as_deref(),
        Some(child_opening.as_str())
    );
    let mut senders: Vec<_> = child_view
        .senders
        .iter()
        .map(|turn| (turn.request_id.as_str(), turn.sender.session_id.as_str()))
        .collect();
    senders.sort_unstable();
    let mut expected = vec![
        (child_opening.as_str(), "sess_parent"),
        (child_message.as_str(), "sess_parent"),
    ];
    expected.sort_unstable();
    assert_eq!(senders, expected);
    let message_sender = child_view
        .senders
        .iter()
        .find(|turn| &turn.request_id == child_message)
        .expect("canonical lineage attributes the message");
    assert_eq!(
        &message_sender.sender.cause_request_doc_id, parent_doc,
        "the message remains attached to its exact physical causing request"
    );
    assert_eq!(message_sender.sender.session_id, "sess_parent");
    let message_call = parent_view
        .calls
        .iter()
        .find(|call| call.tool_call_id == "tc_message")
        .expect("the causing agent-message call remains visible");
    assert_eq!(&message_call.caused.request_id, child_message);

    let existing_view = session_provenance(&desktop.core, fixture.existing())
        .await
        .expect("existing provenance");
    assert!(existing_view.started_by.is_none());
    assert_eq!(sessions(&existing_view.received), ["sess_parent"]);

    let unrelated_view = session_provenance(&desktop.core, fixture.unrelated())
        .await
        .expect("unrelated provenance");
    assert!(unrelated_view.started_by.is_none());
    assert!(
        unrelated_view.received.is_empty()
            && unrelated_view.sent.is_empty()
            && unrelated_view.started.is_empty()
    );
    assert!(unrelated_view.calls.is_empty() && unrelated_view.senders.is_empty());

    let wire_command = json!({
        "name": "desktop_session_provenance", "args": { "request": {
            "nodeDid": desktop.node_did(),
            "sessionId": "sess_parent",
            "requesterDid": desktop.user_did()
        }}
    });
    let command: protocol::ClientCommand = serde_json::from_value(wire_command.clone()).unwrap();
    assert!(command.is_cancellable());
    let host = Arc::new(ClientHost::new(desktop.core.clone()));
    let (mut client, hello, task) = attach_host(host, CancellationToken::new()).await;
    assert!(hello["commands"]
        .as_array()
        .unwrap()
        .contains(&json!("desktop_session_provenance")));
    let response = call_host(&mut client, 41, wire_command).await;
    assert!(response["error"].is_null(), "{response}");
    let observed = &response["result"];
    assert_eq!(observed["sessionId"], "sess_parent");
    let started = observed["started"].as_array().unwrap();
    assert_eq!(started.len(), 1);
    assert!(started
        .iter()
        .all(|link| link["causeRequestDocId"] == *parent_doc));
    assert!(started
        .iter()
        .any(|link| link["sessionId"] == child_session.as_str()));
    drop(client);
    task.await.unwrap().unwrap();

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
