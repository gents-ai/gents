use std::sync::Arc;
use std::time::Duration;

use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::lifecycle::{
    pending_user_queue, replace_pending_user_messages, PendingMessageEdit, PendingQueueEdit,
};
use gents::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents::{Gents, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{
    QueueDelivery, QueuePolicy, QueuePosition, QueueSource, RequestQueue,
};
use tokio::sync::Notify;

use crate::queue_management::{enqueue, insert_signed, row};
use crate::support::fixtures::bind_agent_backend;
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{create_session_document, session_document, test_db};

const AGENT: &str = "steering-engineer";
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

async fn steering_case(hold_tool: bool) {
    let name = if hold_tool {
        "steering-held-tool"
    } else {
        "steering-terminal-arrival"
    };
    let db = test_db(name).await;
    let did = db.node_identity.did();
    let access = ConfigAccess::Local(db.node.clone());
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
    bind_agent_backend(
        db.node.as_ref(),
        did,
        AGENT,
        BACKEND,
        backend.endpoint(),
        MODEL,
    )
    .await;
    let mut session = session_document(name, AGENT, &chrono::Utc::now().to_rfc3339());
    session.node_did = did.into();
    session.requester_did = Some(did.into());
    session.title = Some(gents_protocol::session::SessionTitle {
        text: "Steering test".into(),
        source: gents_protocol::session::SessionTitleSource::Generated,
    });
    create_session_document(db.node.as_ref(), &session).await;
    let active = enqueue(
        &db,
        AGENT,
        name,
        "active-steering-request",
        name,
        QueueDelivery::Steer,
    )
    .await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let runtime = Gents::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_agent_id(AGENT)
        .tool_ceiling(gents::ToolCeiling::meta_only())
        .agent(AGENT)
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
    let active_before = row(&db, &active).await;
    assert_eq!(active_before["lifecycle_state"], "processing");
    let old = enqueue(
        &db,
        AGENT,
        name,
        "old-steering",
        "withdrawn steering content",
        QueueDelivery::Steer,
    )
    .await;
    let cancelled = enqueue(
        &db,
        AGENT,
        name,
        "cancelled-steering",
        "cancelled steering content",
        QueueDelivery::Steer,
    )
    .await;
    let snapshot = pending_user_queue(&access, did, name, did).await.unwrap();
    let ids = snapshot
        .entries
        .iter()
        .map(|entry| entry.request_doc_id.clone())
        .collect::<Vec<_>>();
    let correction = format!("{name}: accepted correction");
    let replacement = replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        name,
        did,
        PendingQueueEdit {
            expected_request_doc_ids: ids,
            selected_request_doc_ids: vec![old.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: old.clone(),
                content: correction.clone(),
            }],
        },
    )
    .await
    .unwrap();
    let snapshot = pending_user_queue(&access, did, name, did).await.unwrap();
    replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        name,
        did,
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
    assert_eq!(row(&db, &old).await["lifecycle_state"], "superseded");
    assert_eq!(row(&db, &cancelled).await["lifecycle_state"], "interrupted");
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
        wait_for_request_terminal(
            db.node.as_ref(),
            "active-steering-request",
            Duration::from_secs(60)
        )
        .await,
        "completed"
    );
    assert_eq!(row(&db, &active).await["_docID"], active);
    let consumed = row(&db, &replacement.request_doc_ids[0]).await;
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
    let captures = access.execute(&format!(r#"{{ RenderedRequest(filter: {{ request_id: {{ _eq: "active-steering-request" }} }}) {{ request_doc_id turn_index }} }}"#)).await.unwrap();
    let captures = captures["data"]["RenderedRequest"].as_array().unwrap();
    assert_eq!(captures.len(), 2);
    assert!(captures
        .iter()
        .all(|capture| capture["request_doc_id"] == active));
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_after_held_tool_uses_same_physical_request_and_excludes_edited_cancelled_input() {
    steering_case(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_arriving_during_terminal_response_continues_same_physical_request() {
    steering_case(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_request_cannot_forge_another_requests_queue_slot() {
    let db = test_db("forged-queue-slot").await;
    let did = db.node_identity.did();
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
    bind_agent_backend(
        db.node.as_ref(),
        did,
        AGENT,
        BACKEND,
        backend.endpoint(),
        MODEL,
    )
    .await;
    let mut session = session_document(session_id, AGENT, &chrono::Utc::now().to_rfc3339());
    session.node_did = did.into();
    session.requester_did = Some(did.into());
    session.title = Some(gents_protocol::session::SessionTitle {
        text: "Slot test".into(),
        source: gents_protocol::session::SessionTitleSource::Generated,
    });
    create_session_document(db.node.as_ref(), &session).await;
    let legitimate = enqueue(
        &db,
        AGENT,
        session_id,
        "legitimate-slot",
        "legitimate slot owner",
        QueueDelivery::Queue,
    )
    .await;
    let mut forged = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "forged-slot",
        did,
        did,
        AGENT,
        session_id,
        "forged content must not execute",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(did),
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
    let forged_id = insert_signed(&db, forged).await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    let runtime = BootedAgent::new(shutdown, handle, did.into());
    assert_eq!(
        wait_for_request_terminal(db.node.as_ref(), "legitimate-slot", Duration::from_secs(60))
            .await,
        "completed"
    );
    assert_eq!(
        wait_for_request_terminal(db.node.as_ref(), "forged-slot", Duration::from_secs(60)).await,
        "failed"
    );
    let forged = row(&db, &forged_id).await;
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
    assert_eq!(row(&db, &legitimate).await["lifecycle_state"], "completed");
    assert_eq!(backend.observed_completion_requests(), 1);
    assert!(backend
        .observed_completion_bodies()
        .iter()
        .all(|body| !body.to_string().contains("forged content must not execute")));
    let captures = ConfigAccess::Local(db.node.clone()).execute(&format!(r#"{{ RenderedRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ request_doc_id }} }}"#, escape_graphql_string("forged-slot"))).await.unwrap();
    assert!(captures["data"]["RenderedRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    runtime.shutdown().await;
}
