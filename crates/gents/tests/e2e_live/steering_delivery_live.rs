use std::time::Duration;

use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{
    QueueDelivery, QueuePolicy, QueuePosition, QueueSource, RequestQueue,
};
use gents_session::request::lifecycle::{
    pending_steer_edit, pending_user_queue, submit_pending_user_steer,
};
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use serde_json::Value;

use crate::support::agent_requests::{canonical_now, submit_user_request, RequestActors};
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, terminal_assistant_answer, wait_for_request_terminal,
};
use crate::support::test_db;

async fn request_row(actors: &RequestActors, doc_id: &str) -> Value {
    let response = actors
        .fixture
        .access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{
            _docID lifecycle_state admission_signature superseded_by_request_doc_id
        }} }}"#,
            escape_graphql_string(doc_id),
        ))
        .await
        .unwrap();
    response["data"]["AgentRequest"][0].clone()
}

async fn submit_signed(actors: &RequestActors, mut request: AgentRequestCreate) -> String {
    request.session_doc_id = Some(
        gents_session::request::lifecycle::reserve_goal_session(&actors.fixture.access, &request)
            .await
            .unwrap(),
    );
    request.agent_doc_id = Some(
        actors
            .fixture
            .access
            .transact_readonly("test.live_steering.agent", |txn| {
                let request = &request;
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents_documents::read_desired_state_record_in_txn(
                            txn,
                            Collection::Agent,
                            &request.node_did,
                            &request.agent_did,
                        )
                        .await?
                        .unwrap()
                        .0,
                    )
                })
            })
            .await
            .unwrap(),
    );
    gents_session::request::admission::sign_agent_request_create(
        &actors.fixture.user,
        &mut request,
    )
    .await
    .unwrap();
    gents_session::request::lifecycle::submit_client_request(&actors.fixture.access, &request)
        .await
        .unwrap();
    let response = actors
        .fixture
        .access
        .execute(&format!(
            r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
            escape_graphql_string(&request.request_id),
        ))
        .await
        .unwrap();
    response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicitly_promoted_queue_input_reaches_the_same_request_after_a_tool() {
    let target = live_target().await;
    let db = test_db("live-promoted-queue-input").await;
    let actors = bind_target(&db, &target).await;
    let (runtime, held) = super::queued_user_messages::boot_held_turn(&db, &actors, &target).await;
    const SESSION: &str = "live-promoted-queue";
    let active = submit_user_request(
        actors.node_did(),
        &actors.fixture,
        SESSION,
        "promote-active",
        "Call hold_turn now. After it returns, acknowledge the newest user instruction briefly.",
        QueueDelivery::Queue,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(60), held.wait())
        .await
        .expect("the live model reaches the held tool");
    let selected = submit_user_request(
        actors.node_did(),
        &actors.fixture,
        SESSION,
        "promote-selected",
        "Acknowledge this promoted instruction briefly. Do not call tools.",
        QueueDelivery::Queue,
    )
    .await;
    submit_user_request(
        actors.node_did(),
        &actors.fixture,
        SESSION,
        "promote-queued",
        "Acknowledge this queued message briefly.",
        QueueDelivery::Queue,
    )
    .await;
    let snapshot = pending_user_queue(
        &actors.fixture.access,
        actors.node_did(),
        SESSION,
        actors.fixture.user.did(),
    )
    .await
    .unwrap();
    let edit = pending_steer_edit(
        &snapshot,
        snapshot
            .entries
            .iter()
            .map(|entry| entry.request_doc_id.clone())
            .collect(),
        &selected,
    )
    .unwrap();
    let receipt = submit_pending_user_steer(
        &actors.fixture.access,
        &actors.fixture.user,
        AgentRequestAdmissionRecord::client(actors.fixture.user.did()),
        actors.node_did(),
        SESSION,
        edit,
        &selected,
        Some("promote-steered"),
    )
    .await
    .unwrap();
    let promoted = &receipt.request_doc_ids[0];
    let replaced = request_row(&actors, &selected).await;
    assert_eq!(replaced["lifecycle_state"], "superseded");
    assert_eq!(replaced["superseded_by_request_doc_id"], promoted.as_str());
    let pending = pending_user_queue(
        &actors.fixture.access,
        actors.node_did(),
        SESSION,
        actors.fixture.user.did(),
    )
    .await
    .unwrap();
    let queued = pending
        .entries
        .iter()
        .find(|entry| entry.content == "Acknowledge this queued message briefly.")
        .expect("promotion retains the ordinary queued message")
        .clone();
    assert_eq!(
        request_row(&actors, &queued.request_doc_id).await["lifecycle_state"],
        "pending"
    );
    held.release();
    for id in ["promote-active", queued.request_id.as_str()] {
        assert_eq!(
            wait_for_request_terminal(&actors.fixture.access, id, Duration::from_secs(60)).await,
            "completed"
        );
    }
    let consumed = request_row(&actors, promoted).await;
    assert_eq!(consumed["lifecycle_state"], "superseded");
    assert_eq!(consumed["superseded_by_request_doc_id"], active);
    assert!(
        request_row(&actors, &queued.request_doc_id).await["superseded_by_request_doc_id"]
            .is_null()
    );
    assert!(
        !terminal_assistant_answer(&actors.fixture.access, "promote-active")
            .await
            .trim()
            .is_empty()
    );
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_request_cannot_forge_another_requests_queue_slot() {
    let target = live_target().await;
    let db = test_db("live-forged-queue-slot").await;
    let actors = bind_target(&db, &target).await;
    const SESSION: &str = "live-forged-slot-session";
    let legitimate = submit_user_request(
        actors.node_did(),
        &actors.fixture,
        SESSION,
        "legitimate-slot",
        "Acknowledge this message briefly without calling tools.",
        QueueDelivery::Queue,
    )
    .await;
    let mut forged = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "forged-slot",
        actors.node_did(),
        actors.fixture.user.did(),
        actors.agent_did(),
        SESSION,
        "An invalid request must not take this slot.",
        "interactive",
        canonical_now(),
        AgentRequestAdmissionRecord::client(actors.fixture.user.did()),
    );
    forged.input.queue = Some(RequestQueue {
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        delivery: QueueDelivery::Queue,
        position: Some(QueuePosition {
            slot_request_doc_id: legitimate.clone(),
            replaces_request_doc_id: legitimate.clone(),
        }),
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    let forged_doc = submit_signed(&actors, forged).await;
    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    assert_eq!(
        wait_for_request_terminal(
            &actors.fixture.access,
            "legitimate-slot",
            Duration::from_secs(60)
        )
        .await,
        "completed"
    );
    assert_eq!(
        wait_for_request_terminal(
            &actors.fixture.access,
            "forged-slot",
            Duration::from_secs(60)
        )
        .await,
        "failed"
    );
    let rejected = request_row(&actors, &forged_doc).await;
    assert!(rejected["admission_signature"]
        .as_str()
        .is_some_and(|signature| !signature.is_empty()));
    assert!(rejected["superseded_by_request_doc_id"].is_null());
    let survivor = request_row(&actors, &legitimate).await;
    assert_eq!(survivor["lifecycle_state"], "completed");
    assert!(survivor["superseded_by_request_doc_id"].is_null());
    assert!(
        !terminal_assistant_answer(&actors.fixture.access, "legitimate-slot")
            .await
            .trim()
            .is_empty()
    );
    runtime.shutdown().await;
}
