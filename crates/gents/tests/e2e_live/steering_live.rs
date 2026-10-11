use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::graphql::escape_graphql_string;
use gents::lifecycle::{
    pending_user_queue, replace_pending_user_messages, PendingMessageEdit, PendingQueueEdit,
};
use gents::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents::rendered_request::{decode_capture_json, CapturePayloadKind};
use gents::{Collection, Gents, ReasoningEffort, SamplingConfig, ToolCeiling};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::QueueDelivery;
use tokio::sync::Notify;

use crate::support::fixtures::bind_agent_backend;
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::{live_target, wait_for_request_terminal};
use crate::support::{create_session_document, session_document_in_scope, test_db, TestDb};

const AGENT: &str = "live-steering";
const SESSION: &str = "live-steering-session";
const ACTIVE: &str = "live-steering-active";
const FIRST: &str = "retained-alpha-4b29";
const SECOND: &str = "retained-beta-9e31";
const OLD: &str = "obsolete-before-edit-28af";
const DROP: &str = "removed-before-intake-61ed";

#[derive(Clone)]
struct HeldTool {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl ToolDyn for HeldTool {
    fn name(&self) -> String {
        "held_probe".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "held_probe".into(),
                description: "Call exactly once before answering; waits for operator updates."
                    .into(),
                parameters: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            }
        })
    }
    fn call<'a>(&'a self, _: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok("Operator updates are ready. Answer using the latest user instructions; do not call held_probe again.".into())
        })
    }
}
struct Release(Arc<Notify>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

async fn enqueue(db: &TestDb, id: &str, content: &str) -> Result<String> {
    let did = db.node_identity.did();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        id,
        did,
        did,
        AGENT,
        SESSION,
        content,
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(did),
    );
    create.input = gents::lifecycle::prepare_user_message_input(
        &ConfigAccess::Local(db.node.clone()),
        did,
        SESSION,
        create.input,
        QueueDelivery::Steer,
    )
    .await?;
    gents::sign_agent_request_create(db.node_identity.as_ref(), &mut create).await?;
    let result = ConfigAccess::Local(db.node.clone())
        .write(
            "test.live_steering.enqueue",
            &create.graphql_mutation().map_err(anyhow::Error::msg)?,
        )
        .await?;
    gents::graphql::created_doc_id(&result, "AgentRequest")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: requires GENTS_LIVE_OPENAI=1 and GENTS_EVAL_TARGET"]
async fn live_steering_edits_reorders_and_removes_before_same_request_continues() -> Result<()> {
    anyhow::ensure!(
        std::env::var("GENTS_LIVE_OPENAI").as_deref() == Ok("1"),
        "set GENTS_LIVE_OPENAI=1"
    );
    let target = live_target();
    let db = test_db("live-steering").await;
    let did = db.node_identity.did();
    let access = ConfigAccess::Local(db.node.clone());
    bind_agent_backend(
        &db.node,
        did,
        AGENT,
        target.backend_id(),
        target.endpoint(),
        target.model(),
    )
    .await;
    let backend = serde_json::to_value(target.backend(did))?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: backend.clone(),
        update: backend,
    }])?;
    ConfigAccess::transact_local(&db.node, None, "test.live_steering.backend", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    })
    .await?;
    let mut session =
        session_document_in_scope(did, SESSION, AGENT, &chrono::Utc::now().to_rfc3339());
    session.requester_did = Some(did.into());
    session.title = Some(gents_protocol::session::SessionTitle {
        text: "Live steering".into(),
        source: gents_protocol::session::SessionTitleSource::Generated,
    });
    create_session_document(&db.node, &session).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let _release_on_failure = Release(release.clone());
    let runtime = Gents::builder().node(db.node.clone()).identity(db.node_identity.clone()).default_agent_id(AGENT)
        .tool_ceiling(ToolCeiling::meta_only()).agent(AGENT).backend_id(target.backend_id()).model_name(target.model())
        .system_prompt("Call held_probe exactly once before answering. After it returns, use the latest user messages, then give a concise answer. Do not call other tools.")
        .sampling(SamplingConfig { reasoning_effort: Some(ReasoningEffort::High), temperature: Some(1.0), top_p: Some(0.95), ..Default::default() })
        .max_turns(1000).deadline_duration_secs(24*60*60).custom_tool(HeldTool { entered: entered.clone(), release: release.clone() }).done().build().await?;
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(runtime.run(receiver));
    wait_for_runtime_ready(&db.node, did).await;
    let runtime = BootedAgent::new(shutdown, handle, did.into());
    let active = enqueue(
        &db,
        ACTIVE,
        "Call held_probe now. Wait for its result before answering.",
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(300), entered.notified())
        .await
        .expect("live model must invoke held_probe; failure is not a skipped test");
    let old = enqueue(&db, "live-steering-old", OLD).await?;
    let second = enqueue(&db, "live-steering-second", SECOND).await?;
    let dropped = enqueue(&db, "live-steering-drop", DROP).await?;
    let snapshot = pending_user_queue(&access, did, SESSION, did).await?;
    let receipt = replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        SESSION,
        did,
        PendingQueueEdit {
            expected_request_doc_ids: snapshot
                .entries
                .iter()
                .map(|entry| entry.request_doc_id.clone())
                .collect(),
            selected_request_doc_ids: vec![old.clone(), second.clone()],
            messages: vec![
                PendingMessageEdit {
                    request_doc_id: second,
                    content: SECOND.into(),
                },
                PendingMessageEdit {
                    request_doc_id: old,
                    content: FIRST.into(),
                },
            ],
        },
    )
    .await?;
    let snapshot = pending_user_queue(&access, did, SESSION, did).await?;
    replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        SESSION,
        did,
        PendingQueueEdit {
            expected_request_doc_ids: snapshot
                .entries
                .iter()
                .map(|entry| entry.request_doc_id.clone())
                .collect(),
            selected_request_doc_ids: vec![dropped],
            messages: vec![],
        },
    )
    .await?;
    let active_id = escape_graphql_string(ACTIVE);
    let before = access.execute(&format!(r#"{{ RenderedRequest(filter: {{ request_id: {{ _eq: "{active_id}" }} }}) {{ request_doc_id }} }}"#)).await?;
    assert_eq!(
        before["data"]["RenderedRequest"].as_array().unwrap().len(),
        1,
        "held tool prevents another provider call"
    );
    release.notify_one();
    assert_eq!(
        wait_for_request_terminal(&db.node, ACTIVE, Duration::from_secs(600)).await,
        "completed"
    );
    let captures = access.execute(&format!(r#"{{ RenderedRequest(filter: {{ request_id: {{ _eq: "{active_id}" }} }}, order: {{ turn_index: ASC }}) {{ request_doc_id capture_version request_json turn_index }} }}"#)).await?;
    let captures = captures["data"]["RenderedRequest"].as_array().unwrap();
    assert!(
        captures.len() >= 2,
        "same request resumes after tool settlement"
    );
    let mut resumed = false;
    for capture in captures {
        assert_eq!(capture["request_doc_id"], active);
        let body = decode_capture_json(
            &access,
            capture["capture_version"].as_u64().unwrap() as u32,
            capture["request_json"].as_str().unwrap(),
            CapturePayloadKind::RequestBody,
        )
        .await?
        .to_string();
        assert!(
            !body.contains(OLD),
            "obsolete input must never reach provider"
        );
        assert!(
            !body.contains(DROP),
            "removed input must never reach provider"
        );
        if let (Some(beta), Some(alpha)) = (body.find(SECOND), body.find(FIRST)) {
            assert!(
                beta < alpha,
                "retained inputs preserve the edited queue order"
            );
            resumed = true;
        }
    }
    assert!(
        resumed,
        "persisted provider input must contain both retained messages"
    );
    for id in receipt.request_doc_ids {
        let row = access.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state superseded_by_request_doc_id }} }}"#, escape_graphql_string(&id))).await?;
        assert_eq!(
            row["data"]["AgentRequest"][0]["lifecycle_state"],
            "superseded"
        );
        assert_eq!(
            row["data"]["AgentRequest"][0]["superseded_by_request_doc_id"],
            active
        );
    }
    runtime.shutdown().await;
    Ok(())
}
