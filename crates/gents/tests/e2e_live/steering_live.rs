use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::inference::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents::inference::rendered_request::{decode_capture_json, CapturePayloadKind};
use gents::request::lifecycle::{
    pending_user_queue, submit_pending_user_edit, PendingMessageEdit, PendingQueueEdit,
};
use gents::store::graphql::escape_graphql_string;
use gents::{Gents, NodeIdentity, ReasoningEffort, SamplingConfig, ToolCeiling};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::QueueDelivery;
use tokio::sync::Notify;

use crate::support::agent_requests::{submit_user_request, RequestActors};
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::{bind_target, live_target, wait_for_request_terminal};
use crate::support::test_db;

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

async fn enqueue(actors: &RequestActors, id: &str, content: &str) -> Result<String> {
    Ok(submit_user_request(
        actors.node_did(),
        &actors.fixture,
        SESSION,
        id,
        content,
        QueueDelivery::Steer,
    )
    .await)
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
    let actors = bind_target(&db, &target).await;
    let did = actors.node_did();
    let user_did = actors.fixture.user.did();
    let agent_did = actors.agent_did();
    let access = actors.fixture.access.clone();
    // The builder runtime admits only a probed backend; the live target was
    // just checked reachable, so its User records the probe result.
    target.assert_reachable().await;
    gents::inference::backends::registry::set_backend_probe_status_as(
        &access,
        did,
        target.backend_id(),
        "healthy",
    )
    .await?;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let _release_on_failure = Release(release.clone());
    let runtime = Gents::builder().node(db.node.clone()).backend_admission(db.home.backend_admission()).identity(db.node_identity.clone()).default_agent_did(agent_did)
        .tool_ceiling(ToolCeiling::meta_only()).agent(agent_did).identity_binding(actors.fixture.binding.clone()).backend_id(target.backend_id()).model_name(target.model())
        .system_prompt("Call held_probe exactly once before answering. After it returns, use the latest user messages, then give a concise answer. Do not call other tools.")
        .sampling(SamplingConfig { reasoning_effort: Some(ReasoningEffort::High), temperature: Some(1.0), top_p: Some(0.95), ..Default::default() })
        .max_turns(1000).deadline_duration_secs(24*60*60).custom_tool(HeldTool { entered: entered.clone(), release: release.clone() }).done().build().await?;
    let active = enqueue(
        &actors,
        ACTIVE,
        "Call held_probe now. Wait for its result before answering.",
    )
    .await?;
    access
        .transact_write("test.live_steering.title", |txn| {
            Box::pin(async move {
                gents::session::apply_title_in_txn(
                    txn,
                    did,
                    Some(user_did),
                    SESSION,
                    Some("Live steering"),
                    gents_protocol::session::SessionTitleSource::Generated,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
            })
        })
        .await?;
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(runtime.run(receiver));
    wait_for_runtime_ready(&db.node, did).await;
    let runtime = BootedAgent::new(shutdown, handle, did.into());
    tokio::time::timeout(Duration::from_secs(300), entered.notified())
        .await
        .expect("live model must invoke held_probe; failure is not a skipped test");
    let old = enqueue(&actors, "live-steering-old", OLD).await?;
    let second = enqueue(&actors, "live-steering-second", SECOND).await?;
    let dropped = enqueue(&actors, "live-steering-drop", DROP).await?;
    let snapshot = pending_user_queue(&access, did, SESSION, user_did).await?;
    let receipt = submit_pending_user_edit(
        &access,
        &actors.fixture.user,
        AgentRequestAdmissionRecord::client(user_did),
        did,
        SESSION,
        PendingQueueEdit {
            expected_request_doc_ids: snapshot
                .entries
                .iter()
                .map(|entry| entry.request_doc_id.clone())
                .collect(),
            selected_request_doc_ids: vec![old.clone(), second.clone()],
            messages: vec![
                PendingMessageEdit {
                    authored_input: None,
                    request_doc_id: second,
                    content: SECOND.into(),
                },
                PendingMessageEdit {
                    authored_input: None,
                    request_doc_id: old,
                    content: FIRST.into(),
                },
            ],
        },
    )
    .await?;
    let snapshot = pending_user_queue(&access, did, SESSION, user_did).await?;
    submit_pending_user_edit(
        &access,
        &actors.fixture.user,
        AgentRequestAdmissionRecord::client(user_did),
        did,
        SESSION,
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
        wait_for_request_terminal(&access, ACTIVE, Duration::from_secs(600)).await,
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
