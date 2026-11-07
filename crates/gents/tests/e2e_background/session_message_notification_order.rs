use std::sync::Arc;
use std::time::Duration;

use gents::documents::{AgentTools, Tools};
use gents::store::graphql::escape_graphql_string;
use gents::{DocumentRuntimeOptions, Gents, NodeIdentity, ToolCeiling};
use serde_json::json;

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, enqueue_managed_accepted_request, prepare_managed_accepted_turn,
    AcceptedTurnSpec,
};
use crate::support::fixtures::{agent_target, bind_agent_backend, configure_agent_tools_as};
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{managed_agent_fixture, test_db, ManagedAgentFixture, TestDb};

const CHILD_TARGET: &str = "e2e-session-order-child";
const BACKEND_ID: &str = "e2e-session-order-backend";
const MODEL: &str = "e2e-session-order-model";
const RESUME_BACKEND_ID: &str = "e2e-session-order-resume-backend";
const RESUME_MODEL: &str = "e2e-session-order-resume-model";
const SESSION_ID: &str = "e2e-session-order-session";
const PARENT_PROMPT: &str = "e2e-session-order-parent-prompt";
const CHILD_PROMPT: &str = "e2e-session-order-child-prompt";
const CHILD_ANSWER: &str = "e2e-session-order-notification-must-survive";
const RESUMED_PROMPT: &str = "e2e-session-order-parent-resumes";

struct TranscriptRow {
    sequence: u32,
    content: String,
}

/// The enrolled User's default Agent delegates to a second named Agent the
/// same User installed; both are managed Agents of this Node.
async fn configure_session_chain(db: &TestDb) -> (ManagedAgentFixture, String) {
    let fixture = managed_agent_fixture(db).await;
    let node_did = db.node_identity.did().to_string();
    let parent_did = fixture.binding.agent_did.clone();
    let (_, child) =
        crate::support::install_managed_agent(db, &fixture.access, &fixture.user, "Order child")
            .await;
    let child_did = child.agent_did;
    crate::support::grant_agent_session_writer(db, &fixture.access, &child_did, &parent_did).await;
    configure_agent_tools_as(
        &fixture.access,
        &node_did,
        &child_did,
        None,
        Tools {
            tools_id: "e2e-session-order-child-tools".into(),
            node_did: node_did.clone(),
            agents: Some(AgentTools {
                target_ids: Vec::new(),
                enabled: Some(false),
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    configure_agent_tools_as(
        &fixture.access,
        &node_did,
        &parent_did,
        None,
        Tools {
            tools_id: "e2e-session-order-parent-tools".into(),
            node_did: node_did.clone(),
            agents: Some(AgentTools {
                target_ids: vec![CHILD_TARGET.into()],
                enabled: Some(true),
            }),
            ..Default::default()
        },
        vec![(
            gents::Collection::AgentTarget,
            serde_json::to_value(agent_target(&node_did, CHILD_TARGET, &node_did, &child_did))
                .expect("serialize the child target"),
        )],
    )
    .await;
    (fixture, child_did)
}

/// The caller session as its configuring User reads it.
async fn session_transcript(db: &TestDb, fixture: &ManagedAgentFixture) -> Vec<TranscriptRow> {
    let session_id = escape_graphql_string(SESSION_ID);
    let response = fixture
        .access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}, order: {{ sequence: ASC }}) {{ _docID }} }}"#
        ))
        .await
        .expect("read caller session messages");
    let header_ids = response["data"]["AgentMessage"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row["_docID"].as_str().expect("message identity").to_owned())
        .collect::<Vec<_>>();
    let mut rows = Vec::with_capacity(header_ids.len());
    for header_id in header_ids {
        let (header, message) =
            crate::support::fixtures::held_canonical_message(db, fixture, &header_id).await;
        rows.push(TranscriptRow {
            sequence: header.sequence,
            content: serde_json::to_string(&message).expect("serialize message"),
        });
    }
    rows
}

async fn wait_for_transcript_marker(db: &TestDb, fixture: &ManagedAgentFixture, marker: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !session_transcript(db, fixture)
        .await
        .iter()
        .any(|row| row.content.contains(marker))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {marker} in the caller session"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_prompt_sorts_after_an_observer_appended_notification() {
    let db = test_db("e2e-session-order").await;
    let node_did = db.node_identity.did().to_string();
    let (fixture, child_did) = configure_session_chain(&db).await;
    let parent_did = fixture.binding.agent_did.clone();

    let prepared = prepare_managed_accepted_turn(
        &db,
        &fixture,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: MODEL,
            parent_agent_did: &parent_did,
            configured_agent_dids: &[&parent_did, &child_did],
            request_id: "e2e-session-order-request",
            session_id: SESSION_ID,
            prompt: PARENT_PROMPT,
            accepted_chunks: vec![StreamChunk::tool_call(
                "e2e-session-order-tool-call",
                gents::tools::native::AGENT_NEW_TOOL_NAME,
                json!({ "agent": CHILD_TARGET, "prompt": CHILD_PROMPT }).to_string(),
            )],
            child_plans: vec![StreamPlan::new(
                CHILD_PROMPT,
                vec![StreamResponse::completes(CHILD_PROMPT, [CHILD_ANSWER])],
            )],
            valid_until: None,
            request_hop: Some(0),
            request_setup: None,
        },
    )
    .await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build first runtime");
    let first = boot_prepared_accepted_turn(&db, prepared, agent).await;
    wait_for_transcript_marker(&db, &fixture, CHILD_ANSWER).await;
    first.shutdown().await;

    let resume_backend = MockStreamingBackend::start(
        RESUME_MODEL,
        vec![StreamScript::completes(RESUMED_PROMPT, ["resumed"])],
    )
    .expect("start resume backend");
    bind_agent_backend(
        &db,
        &parent_did,
        RESUME_BACKEND_ID,
        resume_backend.endpoint(),
        RESUME_MODEL,
    )
    .await;
    enqueue_managed_accepted_request(
        &db,
        &fixture,
        "e2e-session-order-resume-request",
        SESSION_ID,
        RESUMED_PROMPT,
    )
    .await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build resumed runtime");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &node_did).await;
    let resumed = BootedAgent::new(shutdown_tx, handle, node_did.clone());
    wait_for_transcript_marker(&db, &fixture, RESUMED_PROMPT).await;
    resumed.shutdown().await;

    let transcript = session_transcript(&db, &fixture).await;
    let notification = transcript
        .iter()
        .find(|row| row.content.contains(CHILD_ANSWER))
        .expect("completion notification");
    let prompt = transcript
        .iter()
        .find(|row| row.content.contains(RESUMED_PROMPT))
        .expect("resumed authored prompt");
    assert!(
        prompt.sequence > notification.sequence,
        "resumed prompt {} must follow notification {}",
        prompt.sequence,
        notification.sequence
    );
}
