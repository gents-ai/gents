//! ClientSubmissionContention.tla: a User's signed submission reserves its
//! session and selected Agent before signing. A configuration write to that
//! Agent landing between the reservation's read and its commit makes DefraDB
//! abort the commit ("a partial read lost to a newer merge"); the reservation
//! re-reads, re-plans and re-signs after the confirmed conflict and creates
//! the session once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use gents::store::graphql::escape_graphql_string;
use gents::NodeIdentity;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};

use crate::support::agent_requests::{canonical_now, RequestActors};
use crate::support::commit_race_proxy::{is_commit, CommitRaceProxy, ProxyHook};
use crate::support::test_db;

const SESSION: &str = "contended-client-session";

/// The model's `Publications`: concurrent writes landing in the reservation's
/// attempt series, fewer than its attempts.
const PUBLICATIONS: usize = 2;

/// The User renames the selected Agent through the signed desired-state owner.
async fn rename_agent(actors: &RequestActors, name: String) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    let node_did = actors.node_did().to_owned();
    let agent_did = actors.agent_did().to_owned();
    actors
        .fixture
        .access
        .transact_write("test.contention.rename_agent", |txn| {
            let (node_did, agent_did, name) = (&node_did, &agent_did, &name);
            Box::pin(async move {
                let (_, mut agent) = read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .expect("selected Agent");
                agent["display_name"] = name.as_str().into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::Agent,
                    add: agent.clone(),
                    update: agent,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .expect("rename the selected Agent as its User");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reservation_rebuilds_after_a_concurrent_agent_write() {
    let db = test_db("client-submission-contention").await;
    let actors = Arc::new(RequestActors::new(&db).await);
    let node_did = db.node_identity.did().to_owned();
    let user = &actors.fixture.user;

    let commits = Arc::new(AtomicUsize::new(0));
    let hook: ProxyHook = {
        let actors = actors.clone();
        let commits = commits.clone();
        Arc::new(move |method: String, path: String| {
            let actors = actors.clone();
            let commits = commits.clone();
            Box::pin(async move {
                if !is_commit(&method, &path) {
                    return;
                }
                let landed = commits.fetch_add(1, Ordering::SeqCst);
                if landed < PUBLICATIONS {
                    rename_agent(&actors, format!("Renamed {landed}")).await;
                }
            })
        })
    };
    let proxy = CommitRaceProxy::spawn(db.client_endpoint(), hook).await;
    let contended = proxy.client_access(user);
    let request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "contended-request",
        &node_did,
        user.did(),
        actors.agent_did(),
        SESSION,
        "submitted while the Agent is reconfigured",
        "interactive",
        canonical_now(),
        AgentRequestAdmissionRecord::client(user.did()),
    );

    let reserved = gents::request::lifecycle::reserve_goal_session(&contended, &request)
        .await
        .expect("the reservation settles despite the concurrent Agent writes");

    assert_eq!(
        commits.load(Ordering::SeqCst),
        PUBLICATIONS + 1,
        "each overtaken attempt aborted and was rebuilt once"
    );
    let rows = actors
        .fixture
        .access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID requester_did agent_did }} }}"#,
            escape_graphql_string(SESSION),
        ))
        .await
        .unwrap();
    let rows = rows["data"]["AgentSession"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "the rebuild created one session: {rows:?}");
    assert_eq!(rows[0]["_docID"], reserved.as_str());
    assert_eq!(rows[0]["requester_did"], user.did());
    assert_eq!(rows[0]["agent_did"], actors.agent_did());
}
