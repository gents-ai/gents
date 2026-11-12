use std::sync::Arc;
use std::time::Duration;

use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use tokio::sync::{watch, Notify};

use crate::support::agent_requests::submit_user_request;
use crate::support::interrupt::spawn_runtime;
use crate::support::live_inference::{bind_target, live_target, wait_for_request_terminal};
use crate::support::test_db;

#[derive(Clone)]
struct Gate {
    entered: Arc<Notify>,
    release: watch::Sender<bool>,
}

impl ToolDyn for Gate {
    fn name(&self) -> String {
        "hold_turn".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "hold_turn".into(),
                description: "Wait for the user to finish submitting messages.".into(),
                parameters: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            }
        })
    }
    fn call<'a>(&'a self, _: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release
                .subscribe()
                .wait_for(|open| *open)
                .await
                .expect("queue gate owner");
            Ok("The user has finished submitting messages. Acknowledge briefly; do not call hold_turn again.".into())
        })
    }
}

struct Release(watch::Sender<bool>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_queued_behind_a_busy_turn_run_as_separate_turns_in_order() {
    let target = live_target().await;
    let db = test_db("live-queued-user-messages").await;
    let actors = bind_target(&db, &target).await;
    gents_inference::backends::registry::set_backend_probe_status_as(
        &actors.fixture.access,
        actors.node_did(),
        target.backend_id(),
        "healthy",
    )
    .await
    .unwrap();
    let entered = Arc::new(Notify::new());
    let (release, _) = watch::channel(false);
    let _release = Release(release.clone());
    let built = gents::Gents::builder()
        .node(db.node.clone())
        .identity(db.node_identity.clone())
        .backend_admission(db.home.backend_admission())
        .default_agent_did(actors.agent_did())
        .agent(actors.agent_did())
        .identity_binding(actors.fixture.binding.clone())
        .backend_id(target.backend_id())
        .model_name(target.model())
        .system_prompt(
            "Answer briefly. Call hold_turn only when the user explicitly asks you to call it.",
        )
        .custom_tool(Gate {
            entered: entered.clone(),
            release: release.clone(),
        })
        .done()
        .build()
        .await
        .unwrap();
    let runtime = spawn_runtime(&db, built).await;
    let mut documents = Vec::new();
    documents.push(
        submit_user_request(
            actors.node_did(),
            &actors.fixture,
            "live-queue",
            "active",
            "Call hold_turn now. After it returns, acknowledge briefly.",
            Default::default(),
        )
        .await,
    );
    tokio::time::timeout(Duration::from_secs(120), entered.notified())
        .await
        .expect("live model did not reach the held tool");
    for (id, prompt) in [
        ("first", "Acknowledge the first queued message briefly."),
        ("second", "Acknowledge the second queued message briefly."),
    ] {
        documents.push(
            submit_user_request(
                actors.node_did(),
                &actors.fixture,
                "live-queue",
                id,
                prompt,
                Default::default(),
            )
            .await,
        );
    }
    release.send_replace(true);
    for id in ["active", "first", "second"] {
        assert_eq!(
            wait_for_request_terminal(&actors.fixture.access, id, Duration::from_secs(120)).await,
            "completed"
        );
    }
    let response = actors.fixture.access.execute(r#"{ AgentRequest { _docID request_id superseded_by_request_doc_id }
        AgentMessage(filter: {role: {_eq: "assistant"}}, order: {sequence: ASC}) {request_doc_id sequence} }"#).await.unwrap();
    for doc in &documents {
        let row = response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["_docID"] == *doc)
            .unwrap();
        assert!(row["superseded_by_request_doc_id"].is_null());
    }
    let rows = response["data"]["AgentMessage"].as_array().unwrap();
    let sequences = documents
        .iter()
        .map(|doc| {
            rows.iter()
                .filter(|row| row["request_doc_id"] == *doc)
                .filter_map(|row| row["sequence"].as_u64())
                .min()
                .expect("request has a retained assistant response")
        })
        .collect::<Vec<_>>();
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "queued replies are out of order: {sequences:?}"
    );
    runtime.shutdown().await;
}
