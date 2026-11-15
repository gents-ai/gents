//! InterruptLatchContention.tla: a User interrupts a request while the turn
//! writes that request's document. A write landing between the latch's read
//! and its commit makes DefraDB abort the commit; the latch re-reads and
//! re-signs after the confirmed conflict, so the interrupt is accepted and the
//! row keeps one timestamp.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use gents_protocol::operations::request::{RequestInterruptInput, RequestRef};

use crate::support::agent_requests::{submit_user_request, RequestActors};
use crate::support::commit_race_proxy::{is_commit, CommitRaceProxy, ProxyHook};
use crate::support::test_db;

const SESSION: &str = "contended-interrupt-session";
const REQUEST: &str = "contended-interrupt-request";

/// The model's `Writes`: lease renewals landing in the latch's attempt
/// series, fewer than its attempts.
const RENEWALS: usize = 2;

const OTHER_STAMP: &str = "2026-01-01T00:00:00Z";

/// The working Agent writes the request row before each of the latch's
/// first `writes` commits reaches DefraDB.
fn racing_writes<F>(
    actors: &Arc<RequestActors>,
    doc: &str,
    writes: usize,
    write: F,
) -> (ProxyHook, Arc<AtomicUsize>)
where
    F: Fn(usize) -> serde_json::Value + Send + Sync + 'static,
{
    let commits = Arc::new(AtomicUsize::new(0));
    let write = Arc::new(write);
    let hook: ProxyHook = {
        let actors = actors.clone();
        let commits = commits.clone();
        let doc = doc.to_owned();
        Arc::new(move |method: String, path: String| {
            let (actors, commits, doc, write) =
                (actors.clone(), commits.clone(), doc.clone(), write.clone());
            Box::pin(async move {
                if !is_commit(&method, &path) {
                    return;
                }
                let landed = commits.fetch_add(1, Ordering::SeqCst);
                if landed < writes {
                    actors.update_request(&doc, write(landed)).await;
                }
            })
        })
    };
    (hook, commits)
}

/// The User's signed operator access through the proxy, composed with the
/// lifecycle hooks as the desktop's operator access is.
fn operator_access(
    proxy: &CommitRaceProxy,
    actors: &RequestActors,
) -> gents_store::config_client::ConfigAccess {
    match proxy.client_access(&actors.fixture.user) {
        gents_store::config_client::ConfigAccess::Graphql(endpoint) => {
            gents_store::config_client::ConfigAccess::Graphql(
                gents::runtime::lifecycle_hooks::compose_endpoint(endpoint)
                    .expect("compose lifecycle hooks"),
            )
        }
        _ => unreachable!("the proxy hands out GraphQL access"),
    }
}

fn interrupt_input(node_did: &str) -> RequestInterruptInput {
    RequestInterruptInput {
        request: RequestRef {
            node_did: node_did.to_owned(),
            requester_did: None,
            request_id: REQUEST.into(),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_rebuilds_after_concurrent_lease_renewals() {
    let db = test_db("interrupt-latch-contention").await;
    let actors = Arc::new(RequestActors::new(&db).await);
    let node_did = db.node_identity.did().to_owned();
    let doc = submit_user_request(
        &node_did,
        &actors.fixture,
        SESSION,
        REQUEST,
        "interrupted while the turn renews its lease",
        Default::default(),
    )
    .await;
    let (hook, commits) = racing_writes(&actors, &doc, RENEWALS, |landed| {
        serde_json::json!({
            "execution_lease_expires_at": format!("2099-01-01T00:00:0{landed}Z"),
        })
    });
    let proxy = CommitRaceProxy::spawn(db.client_endpoint(), hook).await;
    let contended = operator_access(&proxy, &actors);

    let receipt = gents::operations::request::interrupt(&contended, interrupt_input(&node_did))
        .await
        .expect("the interrupt settles despite the concurrent renewals");

    assert_eq!(
        commits.load(Ordering::SeqCst),
        RENEWALS + 1,
        "each overtaken attempt aborted and was rebuilt once"
    );
    let row = actors.request_row(&doc, "interrupt_requested_at").await;
    let stamped = row["interrupt_requested_at"].as_str().expect("latched");
    assert_eq!(receipt.interrupt_requested_at.as_deref(), Some(stamped));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_rebuild_keeps_another_interrupters_latch() {
    let db = test_db("interrupt-latch-stamped-once").await;
    let actors = Arc::new(RequestActors::new(&db).await);
    let node_did = db.node_identity.did().to_owned();
    let doc = submit_user_request(
        &node_did,
        &actors.fixture,
        SESSION,
        REQUEST,
        "interrupted by two actors at once",
        Default::default(),
    )
    .await;
    let (hook, commits) = racing_writes(&actors, &doc, 1, |_| {
        serde_json::json!({ "interrupt_requested_at": OTHER_STAMP })
    });
    let proxy = CommitRaceProxy::spawn(db.client_endpoint(), hook).await;
    let contended = operator_access(&proxy, &actors);

    let receipt = gents::operations::request::interrupt(&contended, interrupt_input(&node_did))
        .await
        .expect("the interrupt settles on the other interrupter's latch");

    assert_eq!(commits.load(Ordering::SeqCst), 2);
    let row = actors.request_row(&doc, "interrupt_requested_at").await;
    assert_eq!(row["interrupt_requested_at"], OTHER_STAMP);
    assert_eq!(receipt.interrupt_requested_at.as_deref(), Some(OTHER_STAMP));
}
