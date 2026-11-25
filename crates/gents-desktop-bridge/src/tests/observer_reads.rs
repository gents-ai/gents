use super::support::RunningDesktop;
use gents_inference::test_support::scripted_backend::{StreamPlan, StreamResponse};
use gents_observe::client_projection::observe::RuntimeConnectionStatus;

/// `ObserverResync.deliver` under the desktop's own observer: in one plain
/// turn (one provider call, a short answer, no tools) the request's changes
/// read its session scope, so the node is read in full only for a change
/// that is not a session request's: here a change of a session document or a
/// subscription renewal. The session changes are counted on the test's own
/// subscription up to a session it creates after reading the load count, so
/// every change the observer had seen by then is counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_turns_request_changes_read_no_node_snapshot() {
    let marker = "PLAIN-TURN";
    let desktop = RunningDesktop::start(vec![StreamPlan::current_authored_user(
        marker,
        vec![StreamResponse::completes(marker, ["a short answer"])],
    )])
    .await;
    let node = desktop.node_did().to_owned();
    let mut updates = desktop.core.store_updates();
    let deadline = std::time::Duration::from_secs(60);
    tokio::time::timeout(deadline, async {
        while desktop.core.stores().runtime_connection(&node)
            != Some(RuntimeConnectionStatus::Connected)
        {
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("the desktop connects to its runtime");

    let access = desktop.runtime.user_graphql_access();
    let mut sessions = access
        .subscribe("subscription { AgentSession(showDeleted: true) { _docID session_id } }")
        .await
        .unwrap();
    let before = desktop.core.observer_metrics().await.unwrap();
    let doc = desktop.send("sess_plain", "req_plain", marker).await;
    desktop.wait_for_request_state(&doc, "completed").await;
    let finished = || {
        let store = desktop.core.stores().snapshot(&node);
        let request = store
            .request_row("req_plain")
            .is_some_and(|row| row.is_terminal());
        let head = store.sessions.iter().any(|session| {
            session.session_id == "sess_plain"
                && session
                    .observation
                    .as_ref()
                    .and_then(|observation| observation.latest_request.as_ref())
                    .is_some_and(|head| {
                        head.request_doc_id == doc && head.lifecycle_state.is_terminal()
                    })
        });
        request && head
    };
    tokio::time::timeout(deadline, async {
        while !finished() {
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("the desktop shows the finished turn");

    let after = desktop.core.observer_metrics().await.unwrap();
    let last = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "session_id": "sess_last", "node_did": node, "requester_did": desktop.user_did(),
        "agent_did": desktop.lead.agent_did, "created_at": "2026-11-01T00:00:00Z",
    }))
    .unwrap();
    access
        .write(
            "test.observer_reads.last",
            &format!("mutation {{ create_AgentSession(input: {last}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    let mut session_changes = 0_u64;
    tokio::time::timeout(deadline, async {
        'changes: loop {
            let envelope = sessions.recv().await.unwrap().unwrap();
            for row in envelope["data"]["AgentSession"].as_array().unwrap() {
                if row["session_id"] == "sess_last" {
                    break 'changes;
                }
                session_changes += 1;
            }
        }
    })
    .await
    .expect("the test's subscription reaches its own session");

    let loads = after.scope_reloads - before.scope_reloads;
    let renewals = after.subscription_renewals - before.subscription_renewals;
    assert!(
        loads <= session_changes + renewals,
        "{loads} node loads for {session_changes} session changes and {renewals} renewals"
    );
    desktop.shutdown().await;
}
