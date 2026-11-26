use super::support::RunningDesktop;
use gents_inference::test_support::scripted_backend::{StreamPlan, StreamResponse};
use gents_observe::client_projection::observe::RuntimeConnectionStatus;

/// `ObserverResync.deliver` under the desktop's own observer: in one plain
/// turn (one provider call, a short answer, no tools) the request's and its
/// session's changes read their session scope, so the node is read in full
/// only when the subscriptions are renewed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_turn_reads_no_node_snapshot() {
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
    let loads = after.scope_reloads - before.scope_reloads;
    let renewals = after.subscription_renewals - before.subscription_renewals;
    assert!(
        loads <= renewals,
        "{loads} node loads and {renewals} renewals"
    );
    desktop.shutdown().await;
}
