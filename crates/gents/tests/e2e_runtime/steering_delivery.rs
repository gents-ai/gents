use std::sync::Arc;
use std::time::Duration;

use gents::inference::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents::request::lifecycle::{
    pending_steer_edit, pending_user_queue, submit_pending_user_edit, submit_pending_user_steer,
    PendingMessageEdit, PendingQueueEdit,
};
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents::{Gents, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{
    QueueDelivery, QueuePolicy, QueuePosition, QueueSource, RequestQueue,
};
use tokio::sync::Notify;

use crate::queue_management::{enqueue_owned, insert_signed_as, row_as};
use crate::support::agent_requests::{canonical_now, RequestActors};
use crate::support::agent_transcript;
use crate::support::builder_runtime::{
    builder_actors, fixture_agent, runtime_builder, spawn_runtime,
};
use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::test_db;

const MODEL: &str = "steering-model";
const BACKEND: &str = "steering-backend";

#[derive(Clone)]
struct HeldTool {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl ToolDyn for HeldTool {
    fn name(&self) -> String {
        "held_probe".into()
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "held_probe".into(),
                description: "Wait for the test to release the tool".into(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        })
    }
    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok("held tool settled".into())
        })
    }
}

async fn wait_for_signed_request_terminal(
    access: &ConfigAccess,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let escaped = escape_graphql_string(request_id);
    loop {
        let response = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{ lifecycle_state }} }}"#,
        )).await.expect("read steering request");
        let state = response["data"]["AgentRequest"]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["lifecycle_state"].as_str());
        if gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(state) {
            return state.unwrap().to_owned();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "steering request did not terminalize: {response}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn steering_case(hold_tool: bool, promote: bool) {
    let name = if hold_tool {
        "steering-held-tool"
    } else {
        "steering-terminal-arrival"
    };
    let name = if promote {
        format!("queued-{name}")
    } else {
        name.to_owned()
    };
    let name = name.as_str();
    let db = test_db(name).await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let user_did = fixture.user.did();
    let agent_did = fixture.binding.agent_did.as_str();
    let access = fixture.access.clone();
    let first_response = if hold_tool {
        StreamResponse::streams(
            name,
            vec![StreamChunk::tool_call("held-call", "held_probe", "{}")],
        )
    } else {
        StreamResponse::Stream(StreamScript::paused(name, ["first terminal answer"]))
    };
    let backend = MockStreamingBackend::start_with_plans(
        MODEL,
        vec![StreamPlan::new(
            name,
            vec![
                first_response,
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
                gents::session::apply_title_in_txn(
                    txn,
                    did,
                    Some(user_did),
                    name,
                    Some("Steering test"),
                    gents_protocol::session::SessionTitleSource::Generated,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
            })
        })
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let runtime = Gents::builder()
        .node(db.node.clone())
        .backend_admission(db.home.backend_admission())
        .identity(identity)
        .default_agent_did(agent_did)
        .tool_ceiling(gents::ToolCeiling::meta_only())
        .agent(agent_did)
        .identity_binding(fixture.binding.clone())
        .backend_id(BACKEND)
        .model_name(MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(60)
        .custom_tool(HeldTool {
            entered: entered.clone(),
            release: release.clone(),
        })
        .done()
        .build()
        .await
        .unwrap();
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(runtime.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), did).await;
    let runtime = BootedAgent::new(shutdown, handle, did.into());
    if hold_tool {
        tokio::time::timeout(Duration::from_secs(60), entered.notified())
            .await
            .expect("active request reaches held tool");
    } else {
        backend.wait_for_chunks(name, 1).await;
    }
    let active_before = row_as(&fixture, &active).await;
    assert_eq!(active_before["lifecycle_state"], "processing");
    let old = enqueue_owned(
        &db,
        &fixture,
        name,
        "old-steering",
        "withdrawn steering content",
        if promote {
            QueueDelivery::Queue
        } else {
            QueueDelivery::Steer
        },
    )
    .await;
    let cancelled = enqueue_owned(
        &db,
        &fixture,
        name,
        "cancelled-steering",
        "cancelled steering content",
        if promote {
            QueueDelivery::Queue
        } else {
            QueueDelivery::Steer
        },
    )
    .await;
    let snapshot = pending_user_queue(&access, did, name, user_did)
        .await
        .unwrap();
    let ids = snapshot
        .entries
        .iter()
        .map(|entry| entry.request_doc_id.clone())
        .collect::<Vec<_>>();
    let correction = format!("{name}: accepted correction");
    let mut replacement = submit_pending_user_edit(
        &access,
        &fixture.user,
        AgentRequestAdmissionRecord::client(user_did),
        None,
        did,
        name,
        PendingQueueEdit {
            expected_request_doc_ids: ids,
            selected_request_doc_ids: vec![old.clone()],
            messages: vec![PendingMessageEdit {
                authored_input: None,
                request_doc_id: old.clone(),
                content: correction.clone(),
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
        None,
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
    if promote {
        let snapshot = pending_user_queue(&access, did, name, user_did)
            .await
            .unwrap();
        let selected = replacement.request_doc_ids[0].clone();
        assert!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.request_doc_id == selected)
                .unwrap()
                .steerable
        );
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
        let promoted_request_id = uuid::Uuid::new_v4().to_string();
        replacement = submit_pending_user_steer(
            &access,
            &fixture.user,
            AgentRequestAdmissionRecord::client(user_did),
            None,
            did,
            name,
            edit,
            &selected,
            Some(&promoted_request_id),
        )
        .await
        .unwrap();
        assert_eq!(replacement.request_ids.first(), Some(&promoted_request_id));
        assert_eq!(
            row_as(&fixture, &selected).await["lifecycle_state"],
            "superseded"
        );
    }
    assert_eq!(
        row_as(&fixture, &old).await["lifecycle_state"],
        "superseded"
    );
    assert_eq!(
        row_as(&fixture, &cancelled).await["lifecycle_state"],
        "interrupted"
    );
    assert_eq!(
        backend.observed_completion_requests(),
        1,
        "no new provider call while active boundary is held"
    );
    if hold_tool {
        release.notify_one();
    } else {
        backend.release(name);
    }
    assert_eq!(
        wait_for_signed_request_terminal(
            &fixture.access,
            "active-steering-request",
            Duration::from_secs(60)
        )
        .await,
        "completed"
    );
    assert_eq!(row_as(&fixture, &active).await["_docID"], active);
    let consumed = row_as(&fixture, &replacement.request_doc_ids[0]).await;
    assert_eq!(consumed["lifecycle_state"], "superseded");
    assert_eq!(consumed["superseded_by_request_doc_id"], active);
    let bodies = backend.observed_completion_bodies();
    assert_eq!(bodies.len(), 2, "same running request continues once");
    assert!(!bodies[0].to_string().contains(&correction));
    assert!(bodies[1].to_string().contains(&correction));
    for body in &bodies {
        assert!(!body.to_string().contains("withdrawn steering content"));
        assert!(!body.to_string().contains("cancelled steering content"));
    }
    if hold_tool {
        assert!(bodies[1].to_string().contains("held tool settled"));
    } else {
        assert!(bodies[1].to_string().contains("first terminal answer"));
    }
    let captures = access.execute(r#"{ RenderedRequest(filter: { request_id: { _eq: "active-steering-request" } }) { request_doc_id turn_index } }"#).await.unwrap();
    let captures = captures["data"]["RenderedRequest"].as_array().unwrap();
    assert_eq!(captures.len(), 2);
    assert!(captures
        .iter()
        .all(|capture| capture["request_doc_id"] == active));
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_after_held_tool_uses_same_physical_request_and_excludes_edited_cancelled_input() {
    steering_case(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_arriving_during_terminal_response_continues_same_physical_request() {
    steering_case(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicitly_promoted_queue_input_reaches_the_same_request_after_a_tool() {
    steering_case(true, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicitly_promoted_queue_input_continues_the_same_request_after_terminal_text() {
    steering_case(false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_request_cannot_forge_another_requests_queue_slot() {
    let db = test_db("forged-queue-slot").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let user_did = fixture.user.did();
    let agent_did = fixture.binding.agent_did.as_str();
    let session_id = "forged-slot-session";
    let backend = MockStreamingBackend::start_with_plans(
        MODEL,
        vec![StreamPlan::new(
            "legitimate slot owner",
            vec![StreamResponse::completes(
                "legitimate slot owner",
                ["legitimate done"],
            )],
        )],
    )
    .unwrap();
    bind_managed_agent_backend(&db, &fixture, BACKEND, backend.endpoint(), MODEL).await;
    let legitimate = enqueue_owned(
        &db,
        &fixture,
        session_id,
        "legitimate-slot",
        "legitimate slot owner",
        QueueDelivery::Queue,
    )
    .await;
    fixture
        .access
        .transact_write("test.forged_slot.title", |txn| {
            Box::pin(async move {
                gents::session::apply_title_in_txn(
                    txn,
                    did,
                    Some(user_did),
                    session_id,
                    Some("Slot test"),
                    gents_protocol::session::SessionTitleSource::Generated,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
            })
        })
        .await
        .unwrap();
    let mut forged = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "forged-slot",
        did,
        user_did,
        agent_did,
        session_id,
        "forged content must not execute",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user_did),
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
    let forged_id = insert_signed_as(&fixture, forged).await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    let runtime = BootedAgent::new(shutdown, handle, did.into());
    assert_eq!(
        wait_for_signed_request_terminal(
            &fixture.access,
            "legitimate-slot",
            Duration::from_secs(60)
        )
        .await,
        "completed"
    );
    assert_eq!(
        wait_for_signed_request_terminal(&fixture.access, "forged-slot", Duration::from_secs(60))
            .await,
        "failed"
    );
    let forged = row_as(&fixture, &forged_id).await;
    assert_eq!(forged["content"], "forged content must not execute");
    assert!(
        forged["failure_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("queue position")),
        "{forged}"
    );
    assert!(
        forged["admission_signature"]
            .as_str()
            .is_some_and(|signature| !signature.is_empty()),
        "this is signed but unauthorized slot ownership, not a missing signature fixture"
    );
    assert_eq!(
        row_as(&fixture, &legitimate).await["lifecycle_state"],
        "completed"
    );
    assert_eq!(backend.observed_completion_requests(), 1);
    assert!(backend
        .observed_completion_bodies()
        .iter()
        .all(|body| !body.to_string().contains("forged content must not execute")));
    let captures = fixture.access.execute(&format!(
        r#"{{ RenderedRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ request_doc_id }} }}"#,
        escape_graphql_string("forged-slot"),
    )).await.unwrap();
    assert!(captures["data"]["RenderedRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    runtime.shutdown().await;
}

/// A calling Agent's session message into a session it opened on another
/// Agent, as `agent_message` stages one: client admission signed by the
/// caller, caused by the caller's own signed request and tool call.
#[allow(clippy::too_many_arguments)]
async fn caller_message(
    caller: &RequestActors,
    cause: &agent_transcript::Source,
    tool_call_doc: &str,
    tool_call_id: &str,
    target_agent_did: &str,
    session_id: &str,
    request_id: &str,
    content: &str,
    queue: Option<RequestQueue>,
) -> String {
    let caller_did = caller.agent_did();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        caller.node_did(),
        caller_did,
        target_agent_did,
        session_id,
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
        gents::request::lifecycle::reserve_goal_session(&caller.agent_access, &create)
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
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            gents::Collection::Agent,
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
    gents::sign_agent_request_create(caller.agent().as_ref(), &mut create)
        .await
        .expect("sign the caller's session message");
    gents::request::lifecycle::submit_client_request(&caller.agent_access, &create)
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
    db: &crate::support::TestDb,
    actors: &RequestActors,
    entered: &Arc<Notify>,
    release: &Arc<Notify>,
) -> gents::GentsBuilder {
    fixture_agent(runtime_builder(db, actors), actors, actors.agent_did())
        .backend_id(BACKEND)
        .model_name(MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(60)
        .custom_tool(HeldTool {
            entered: entered.clone(),
            release: release.clone(),
        })
        .done()
        .tool_ceiling(gents::ToolCeiling::meta_only())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_agent_steer_passes_a_queued_turn_after_a_held_tool() {
    let name = "agent-steer-past-queue";
    let db = test_db(name).await;
    let backend = MockStreamingBackend::start_with_plans(
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
    // The worker Agent runs the steered session; a second Agent of the same
    // User opened that session and messages it from its own turn.
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
        &open_call,
        "agent-new-call",
        worker.agent_did(),
        name,
        "priority-head",
        name,
        None,
    )
    .await;
    let caller_did = caller.agent_did().to_owned();
    caller
        .agent_access
        .transact_write("test.steering.title", |txn| {
            let node_did = worker.node_did();
            let caller_did = caller_did.as_str();
            Box::pin(async move {
                gents::session::apply_title_in_txn(
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
    let reached = tokio::time::timeout(Duration::from_secs(60), entered.notified()).await;
    assert!(
        reached.is_ok(),
        "head did not reach held tool: request={:?}, provider_requests={}, bodies={:?}",
        worker
            .request_row(&active, "lifecycle_state failure_reason")
            .await,
        backend.observed_completion_requests(),
        backend.observed_completion_bodies(),
    );
    let queued = caller_message(
        &caller,
        &cause,
        &queue_call,
        "agent-message-queue",
        worker.agent_did(),
        name,
        "priority-queued",
        "queued turn",
        Some(steering_queue(QueueDelivery::Queue, "priority-head")),
    )
    .await;
    let steer_doc = caller_message(
        &caller,
        &cause,
        &steer_call,
        "agent-message-steer",
        worker.agent_did(),
        name,
        "priority-steer",
        "steer at tool boundary",
        Some(steering_queue(QueueDelivery::Steer, "priority-head")),
    )
    .await;
    const FIELDS: &str = "lifecycle_state failure_reason superseded_by_request_doc_id";
    assert_eq!(backend.observed_completion_requests(), 1);
    release.notify_one();
    backend.wait_for_chunks(name, 2).await;
    assert_eq!(
        worker.request_row(&queued, FIELDS).await["lifecycle_state"],
        "pending"
    );
    let consumed = worker.request_row(&steer_doc, FIELDS).await;
    assert_eq!(consumed["lifecycle_state"], "superseded", "{consumed}");
    assert_eq!(consumed["superseded_by_request_doc_id"], active);
    let bodies = backend.observed_completion_bodies();
    assert_eq!(bodies.len(), 2);
    assert!(!bodies[0].to_string().contains("steer at tool boundary"));
    assert!(bodies[1].to_string().contains("steer at tool boundary"));
    assert!(bodies[1].to_string().contains("held tool settled"));
    assert!(!bodies[1].to_string().contains("queued turn"));
    runtime.crash().await;
    let runtime = spawn_runtime(
        &db,
        held_runtime(&db, &worker, &entered, &release)
            .build()
            .await
            .unwrap(),
    )
    .await;
    let head_state = wait_for_signed_request_terminal(
        &worker.agent_access,
        "priority-head",
        Duration::from_secs(60),
    )
    .await;
    let head_row = worker.request_row(&active, FIELDS).await;
    assert_eq!(
        head_state,
        "failed",
        "head={head_row} bodies={:?}",
        backend.observed_completion_bodies()
    );
    assert_eq!(
        head_row["failure_reason"], "execution lease expired",
        "head={head_row}"
    );
    let queued_state = wait_for_signed_request_terminal(
        &worker.agent_access,
        "priority-queued",
        Duration::from_secs(60),
    )
    .await;
    let queued_row = worker.request_row(&queued, FIELDS).await;
    assert_eq!(
        queued_state,
        "completed",
        "head={head_row} queued={queued_row} bodies={:?}",
        backend.observed_completion_bodies()
    );
    let bodies = backend.observed_completion_bodies();
    assert!(
        bodies.len() >= 3,
        "queued successor sends a provider request after recovery"
    );
    let successor_bodies = bodies
        .iter()
        .filter(|body| body.to_string().contains("queued turn"))
        .collect::<Vec<_>>();
    assert_eq!(
        successor_bodies.len(),
        1,
        "queued successor dispatches once: {bodies:?}"
    );
    let replay = successor_bodies[0].to_string();
    assert_eq!(
        replay.matches("steer at tool boundary").count(),
        1,
        "the folded input replays exactly once"
    );
    assert!(replay.contains("queued turn"));
    assert_eq!(
        worker.request_row(&steer_doc, FIELDS).await["superseded_by_request_doc_id"],
        active
    );
    backend.release(name);
    runtime.shutdown().await;
}
