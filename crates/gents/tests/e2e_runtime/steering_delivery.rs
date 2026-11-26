use std::sync::Arc;

use gents::test_support::tools::HeldTool;
use gents::Gents;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
use gents_session::request::lifecycle::{
    pending_user_queue, submit_pending_user_edit, PendingMessageEdit, PendingQueueEdit,
};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::UNREACHED_DEADLINE;
use tokio::sync::Notify;

use super::queue_management::{enqueue_owned, row_as};
use crate::support::agent_requests::{canonical_now, RequestActors};
use crate::support::agent_transcript;
use crate::support::builder_runtime::{builder_actors, fixture_agent, runtime_builder};
use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::interrupt::spawn_runtime;
use crate::support::trial_store;
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use gents_store::test_support::STALL_CEILING;

const MODEL: &str = "steering-model";
const BACKEND: &str = "steering-backend";

async fn wait_for_signed_request_terminal(access: &ConfigAccess, request_id: &str) -> String {
    crate::support::live_inference::wait_for_request_terminal(access, request_id).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_arriving_during_terminal_response_continues_same_physical_request() {
    let name = "steering-terminal-arrival";
    let db = trial_store(name).await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let user_did = fixture.user.did();
    let agent_did = fixture.binding.agent_did.as_str();
    let access = fixture.access.clone();
    let backend = ScriptedBackend::start_with_plans(
        MODEL,
        vec![StreamPlan::new(
            name,
            vec![
                StreamResponse::Stream(StreamScript::paused(name, ["first terminal answer"])),
                StreamResponse::completes(name, ["corrected final answer"]),
            ],
        )],
    )
    .unwrap();
    bind_managed_agent_backend(&db, &fixture, BACKEND, backend.endpoint(), MODEL).await;
    let active = enqueue_owned(
        &db,
        &fixture,
        name,
        "active-steering-request",
        name,
        QueueDelivery::Steer,
    )
    .await;
    access
        .transact_write("test.steering.title", |txn| {
            Box::pin(async move {
                gents_session::session::apply_title_in_txn(
                    txn,
                    did,
                    Some(user_did),
                    name,
                    Some("Steering test"),
                    gents_protocol::session::SessionTitleSource::Generated,
                    &canonical_now(),
                )
                .await
            })
        })
        .await
        .unwrap();
    let agent = Gents::builder()
        .node(db.node.clone())
        .backend_admission(db.backend_admission())
        .identity(db.node_identity.clone())
        .default_agent_did(agent_did)
        .tool_ceiling(gents_tools::surface::ToolCeiling::meta_only())
        .agent(agent_did)
        .identity_binding(fixture.binding.clone())
        .backend_id(BACKEND)
        .model_name(MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(UNREACHED_DEADLINE.as_secs())
        .done()
        .build()
        .await
        .unwrap();
    let runtime = spawn_runtime(&db, agent).await;
    backend.wait_for_chunks(name, 1).await;
    assert_eq!(
        row_as(&fixture, &active).await["lifecycle_state"],
        "processing"
    );
    let old = enqueue_owned(
        &db,
        &fixture,
        name,
        "old-steering",
        "withdrawn steering content",
        QueueDelivery::Steer,
    )
    .await;
    let cancelled = enqueue_owned(
        &db,
        &fixture,
        name,
        "cancelled-steering",
        "cancelled steering content",
        QueueDelivery::Steer,
    )
    .await;
    let snapshot = pending_user_queue(&access, did, name, user_did)
        .await
        .unwrap();
    let replacement = submit_pending_user_edit(
        &access,
        &fixture.user,
        AgentRequestAdmissionRecord::client(user_did),
        did,
        name,
        PendingQueueEdit {
            expected_request_doc_ids: snapshot
                .entries
                .iter()
                .map(|entry| entry.request_doc_id.clone())
                .collect(),
            selected_request_doc_ids: vec![old.clone()],
            messages: vec![PendingMessageEdit {
                authored_input: None,
                request_doc_id: old.clone(),
                content: "accepted correction".into(),
            }],
        },
    )
    .await
    .unwrap();
    let snapshot = pending_user_queue(&access, did, name, user_did)
        .await
        .unwrap();
    submit_pending_user_edit(
        &access,
        &fixture.user,
        AgentRequestAdmissionRecord::client(user_did),
        did,
        name,
        PendingQueueEdit {
            expected_request_doc_ids: snapshot
                .entries
                .iter()
                .map(|entry| entry.request_doc_id.clone())
                .collect(),
            selected_request_doc_ids: vec![cancelled.clone()],
            messages: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        row_as(&fixture, &old).await["lifecycle_state"],
        "superseded"
    );
    assert_eq!(
        row_as(&fixture, &cancelled).await["lifecycle_state"],
        "interrupted"
    );
    backend.release(name);
    assert_eq!(
        wait_for_signed_request_terminal(&access, "active-steering-request").await,
        "completed"
    );
    let consumed = row_as(&fixture, &replacement.request_doc_ids[0]).await;
    assert_eq!(consumed["lifecycle_state"], "superseded");
    assert_eq!(consumed["superseded_by_request_doc_id"], active);
    assert!(!crate::support::live_inference::terminal_assistant_answer(
        &access,
        "active-steering-request"
    )
    .await
    .trim()
    .is_empty());
    runtime.shutdown().await;
}

struct ToolCallMessage<'a> {
    tool_call_doc: &'a str,
    tool_call_id: &'a str,
    request_id: &'a str,
    content: &'a str,
    queue: Option<RequestQueue>,
}

async fn caller_message(
    caller: &RequestActors,
    cause: &agent_transcript::Source,
    target_agent_did: &str,
    session_id: &str,
    message: ToolCallMessage<'_>,
) -> String {
    let ToolCallMessage {
        tool_call_doc,
        tool_call_id,
        request_id,
        content,
        queue,
    } = message;
    let caller_did = caller.agent_did();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        RequestScope::new(caller.node_did(), caller_did, target_agent_did, session_id),
        content,
        "interactive",
        canonical_now(),
        AgentRequestAdmissionRecord::client(caller_did),
    );
    create.request_hop = 1;
    create.caused_by_parent_request_id = Some(cause.request_id.clone());
    create.caused_by_parent_request_doc_id = Some(cause.request_doc.clone());
    create.caused_by_parent_tool_call_id = Some(tool_call_id.into());
    create.caused_by_parent_tool_call_doc_id = Some(tool_call_doc.into());
    create.input.queue = queue;
    create.session_doc_id = Some(
        gents_session::request::lifecycle::reserve_goal_session(&caller.agent_access, &create)
            .await
            .expect("reserve the caller's target session"),
    );
    let node_did = caller.node_did().to_owned();
    create.agent_doc_id = Some(
        caller
            .agent_access
            .transact_readonly("test.steering.target_agent", |txn| {
                let node_did = &node_did;
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents_documents::read_desired_state_record_in_txn(
                            txn,
                            gents_store::collection::Collection::Agent,
                            node_did,
                            target_agent_did,
                        )
                        .await?
                        .expect("target Agent")
                        .0,
                    )
                })
            })
            .await
            .expect("read the target Agent as the caller"),
    );
    gents_session::request::admission::sign_agent_request_create(
        caller.agent().as_ref(),
        &mut create,
    )
    .await
    .expect("sign the caller's session message");
    gents_session::request::lifecycle::submit_client_request(&caller.agent_access, &create)
        .await
        .expect("submit the caller's session message");
    let response = caller
        .agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
            escape_graphql_string(request_id),
        ))
        .await
        .expect("read the caller's session message");
    response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .expect("the caller reads its own session message")
        .to_owned()
}

fn steering_queue(delivery: QueueDelivery, after: &str) -> RequestQueue {
    RequestQueue {
        source: QueueSource::Steering,
        policy: QueuePolicy::Append,
        delivery,
        position: None,
        key: None,
        queued_after_request_id: Some(after.into()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

fn held_runtime(
    db: &gents_store::test_support::TestStore,
    actors: &RequestActors,
    entered: &Arc<Notify>,
    release: &Arc<Notify>,
) -> gents::GentsBuilder {
    fixture_agent(runtime_builder(db, actors), actors, actors.agent_did())
        .backend_id(BACKEND)
        .model_name(MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(UNREACHED_DEADLINE.as_secs())
        .custom_tool(HeldTool::notified(
            "held_probe",
            "Wait for the caller to release the tool",
            serde_json::json!({"type":"object","properties":{}}),
            "held tool settled",
            entered.clone(),
            release.clone(),
        ))
        .done()
        .tool_ceiling(gents_tools::surface::ToolCeiling::meta_only())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_agent_steering_survives_a_forced_mid_turn_crash() {
    let name = "agent-steer-past-queue";
    let db = trial_store(name).await;
    let backend = ScriptedBackend::start_with_plans(
        MODEL,
        vec![
            StreamPlan::new(
                name,
                vec![
                    StreamResponse::streams(
                        name,
                        vec![StreamChunk::tool_call("held-call", "held_probe", "{}")],
                    ),
                    StreamResponse::Stream(StreamScript::paused(name, ["head result"])),
                ],
            ),
            StreamPlan::current_authored_user(
                "queued turn",
                vec![StreamResponse::completes("queued turn", ["queued result"])],
            ),
        ],
    )
    .unwrap();
    let worker = builder_actors(&db, BACKEND, backend.endpoint(), MODEL, 1).await;
    let (caller_agent, caller_binding) = crate::support::install_managed_agent(
        &db,
        &worker.fixture.access,
        &worker.fixture.user,
        "Steering caller",
    )
    .await;
    crate::support::grant_agent_session_writer(
        &db,
        &worker.fixture.access,
        worker.agent_did(),
        &caller_binding.agent_did,
    )
    .await;
    let caller = RequestActors::from_fixture(
        &db,
        crate::support::ManagedAgentFixture {
            user: crate::support::load_managed_fixture_user(&db).await,
            access: worker.fixture.access.clone(),
            agent: caller_agent,
            binding: caller_binding,
        },
    )
    .await;
    let cause = agent_transcript::source(&caller, "caller-turn").await;
    let open_call = agent_transcript::tool_call(&caller, &cause, 1, "agent-new-call").await;
    let steer_call = agent_transcript::tool_call(&caller, &cause, 2, "agent-message-steer").await;
    let queue_call = agent_transcript::tool_call(&caller, &cause, 3, "agent-message-queue").await;

    let active = caller_message(
        &caller,
        &cause,
        worker.agent_did(),
        name,
        ToolCallMessage {
            tool_call_doc: &open_call,
            tool_call_id: "agent-new-call",
            request_id: "priority-head",
            content: name,
            queue: None,
        },
    )
    .await;
    let caller_did = caller.agent_did().to_owned();
    caller
        .agent_access
        .transact_write("test.steering.title", |txn| {
            let node_did = worker.node_did();
            let caller_did = caller_did.as_str();
            Box::pin(async move {
                gents_session::session::apply_title_in_txn(
                    txn,
                    node_did,
                    Some(caller_did),
                    name,
                    Some("Agent steering replay"),
                    gents_protocol::session::SessionTitleSource::Generated,
                    &canonical_now(),
                )
                .await
            })
        })
        .await
        .expect("title the caller's session");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = spawn_runtime(
        &db,
        held_runtime(&db, &worker, &entered, &release)
            .build()
            .await
            .unwrap(),
    )
    .await;
    let reached = tokio::time::timeout(STALL_CEILING, entered.notified()).await;
    assert!(reached.is_ok(), "head did not reach the held tool");
    let queued = caller_message(
        &caller,
        &cause,
        worker.agent_did(),
        name,
        ToolCallMessage {
            tool_call_doc: &queue_call,
            tool_call_id: "agent-message-queue",
            request_id: "priority-queued",
            content: "queued turn",
            queue: Some(steering_queue(QueueDelivery::Queue, "priority-head")),
        },
    )
    .await;
    let steer_doc = caller_message(
        &caller,
        &cause,
        worker.agent_did(),
        name,
        ToolCallMessage {
            tool_call_doc: &steer_call,
            tool_call_id: "agent-message-steer",
            request_id: "priority-steer",
            content: "steer at tool boundary",
            queue: Some(steering_queue(QueueDelivery::Steer, "priority-head")),
        },
    )
    .await;
    const FIELDS: &str = "lifecycle_state failure_reason superseded_by_request_doc_id";
    release.notify_one();
    backend.wait_for_chunks(name, 2).await;
    assert_eq!(
        worker.request_row(&queued, FIELDS).await["lifecycle_state"],
        "pending"
    );
    let consumed = worker.request_row(&steer_doc, FIELDS).await;
    assert_eq!(consumed["lifecycle_state"], "superseded", "{consumed}");
    assert_eq!(consumed["superseded_by_request_doc_id"], active);
    runtime.crash().await;
    let runtime = spawn_runtime(
        &db,
        held_runtime(&db, &worker, &entered, &release)
            .build()
            .await
            .unwrap(),
    )
    .await;
    let head_state = wait_for_signed_request_terminal(&worker.agent_access, "priority-head").await;
    let head_row = worker.request_row(&active, FIELDS).await;
    assert_eq!(head_state, "failed", "head={head_row}");
    assert_eq!(
        head_row["failure_reason"], "execution lease expired",
        "head={head_row}"
    );
    let queued_state =
        wait_for_signed_request_terminal(&worker.agent_access, "priority-queued").await;
    let queued_row = worker.request_row(&queued, FIELDS).await;
    assert_eq!(
        queued_state, "completed",
        "head={head_row} queued={queued_row}"
    );
    assert!(!crate::support::live_inference::terminal_assistant_answer(
        &worker.agent_access,
        "priority-queued"
    )
    .await
    .trim()
    .is_empty());
    assert_eq!(
        worker.request_row(&steer_doc, FIELDS).await["superseded_by_request_doc_id"],
        active
    );
    backend.release(name);
    runtime.shutdown().await;
}
