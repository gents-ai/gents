//! Runtime wiring fence for request-local canonical transcript compaction.

use super::*;
use std::{sync::Arc, time::Duration};

use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::support::fixtures::test_identity;
use super::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use super::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse,
};

const MODEL: &str = "default";
const BACKEND_ID: &str = "backend-compaction-gate";
const REQUEST_MARKER: &str = "compaction-gate-request";
const COMPACTION_MARKER: &str = "supplied structured-output schema";
const REUSED_CALL_ID: &str = "provider-call-reused-across-turns";
const SEEDED_TURNS: usize = 12;
const SEEDED_TURN_BYTES: usize = 10_000;

pub(super) async fn compaction_runtime_reduces_valid_canonical_history() {
    let db = test_db("compaction-gate").await;
    let backend = MockStreamingBackend::start_with_plans(
        MODEL,
        vec![
            StreamPlan::new(
                COMPACTION_MARKER,
                vec![StreamResponse::completes(
                    COMPACTION_MARKER,
                    [r#"{"goal":"continue","completed_work":["canonical history summarized"]}"#],
                )],
            ),
            StreamPlan::current_authored_user(
                REQUEST_MARKER,
                vec![
                    StreamResponse::streams(
                        REQUEST_MARKER,
                        vec![StreamChunk::tool_call(
                            REUSED_CALL_ID,
                            "list_processes",
                            r#"{}"#,
                        )],
                    ),
                    StreamResponse::streams(
                        REQUEST_MARKER,
                        vec![StreamChunk::tool_call(
                            REUSED_CALL_ID,
                            "list_processes",
                            r#"{}"#,
                        )],
                    ),
                    StreamResponse::completes(REQUEST_MARKER, ["ok"]),
                ],
            ),
        ],
    )
    .expect("start mock backend");
    let agent = boot_agent(&db, backend.endpoint()).await;
    let session_id = format!("session-{}", uuid::Uuid::new_v4());
    seed_bulky_history(db.node.as_ref(), &agent.agent_did, &session_id).await;

    let first = run_request(&db, &agent, &session_id, "first").await;
    assert_eq!(first.lifecycle_state, RequestLifecycleState::Completed);
    assert!(backend.observed_requests(COMPACTION_MARKER) >= 1);
    assert_eq!(
        tool_call_count(db.node.as_ref(), &session_id, REUSED_CALL_ID).await,
        2,
        "the same provider call id is valid in two distinct request-local turns"
    );
    let first_entries =
        support::snapshots::fetch_compaction_entry_snapshots_for_session(&db.node, &session_id)
            .await;
    let checkpoint = first_entries
        .last()
        .expect("persisted compaction checkpoint");
    let cursor = checkpoint
        .compacted_through_sequence
        .expect("runtime compaction must persist its canonical cursor");
    assert!(checkpoint.messages_compacted > 0);
    assert!(checkpoint.request_doc_id.is_some());
    assert!(cursor < (SEEDED_TURNS as u32) * 2);

    // This request reconstructs authorized provider input from the checkpoint
    // and the remaining canonical tail.
    let second = run_request(&db, &agent, &session_id, "second").await;
    assert_eq!(second.lifecycle_state, RequestLifecycleState::Completed);
    assert!(backend.observed_requests(REQUEST_MARKER) >= 2);
    assert!(
        support::snapshots::fetch_compaction_entry_snapshots_for_session(&db.node, &session_id)
            .await
            .len()
            >= first_entries.len()
    );

    agent.shutdown().await;
}

async fn tool_call_count(node: &EmbeddedNode, session_id: &str, tool_call_id: &str) -> usize {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let tool_call_id = gents::graphql::escape_graphql_string(tool_call_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, tool_call_id: {{ _eq: "{tool_call_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    gents::graphql::rows::<serde_json::Value>(&response, "AgentToolCall")
        .expect("AgentToolCall rows")
        .len()
}

async fn run_request(
    db: &support::TestDb,
    agent: &BootedAgent,
    session_id: &str,
    suffix: &str,
) -> LifecycleStateRow {
    let request_id = format!("{suffix}-{}", uuid::Uuid::new_v4());
    let doc_id = create_runtime_request(
        db.node.as_ref(),
        &agent.agent_did,
        AGENT_NAME,
        &request_id,
        session_id,
        REQUEST_MARKER,
    )
    .await;
    wait_for_terminal_request(db.node.as_ref(), &doc_id).await
}

async fn seed_bulky_history(node: &EmbeddedNode, agent_did: &str, session_id: &str) {
    let mut sequence = 0u32;
    for turn in 0..SEEDED_TURNS {
        let payload = "h".repeat(SEEDED_TURN_BYTES);
        for (role, content) in [
            ("user", format!("turn {turn}: {payload}")),
            ("assistant", format!("reply {turn}: {payload}")),
        ] {
            crate::support::create_agent_message_in_scope(
                node,
                agent_did,
                Some(agent_did),
                session_id,
                sequence,
                role,
                &content,
                &chrono::Utc::now().to_rfc3339(),
            )
            .await;
            sequence += 1;
        }
    }
}

async fn boot_agent(db: &support::TestDb, endpoint: &str) -> BootedAgent {
    let identity: Arc<dyn gents::AgentIdentity> = Arc::new(test_identity("compaction-gate"));
    support::fixtures::bind_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        AGENT_NAME,
        BACKEND_ID,
        endpoint,
        MODEL,
    )
    .await;
    let agent = gents::Gents::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_behavior_id(AGENT_NAME)
        .tool_ceiling(gents::ToolCeiling::meta_only())
        .behavior(AGENT_NAME)
        .backend_id(BACKEND_ID)
        .model_name(MODEL)
        .stream_batch_ms(0)
        .context_window(30_000)
        .compaction_threshold(0.5)
        .done()
        .build()
        .await
        .expect("build compaction agent");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    BootedAgent::new(shutdown_tx, handle, agent_did)
}

async fn wait_for_terminal_request(node: &EmbeddedNode, request_doc_id: &str) -> LifecycleStateRow {
    let doc_id = gents::graphql::escape_graphql_string(request_doc_id);
    let started = std::time::Instant::now();
    loop {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ lifecycle_state }} }}"#
            ))
            .await;
        if let Some(row) = first_optional_row::<LifecycleStateRow>(&response, "AgentRequest") {
            if row.lifecycle_state.is_terminal() {
                return row;
            }
        }
        assert!(started.elapsed() < Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[derive(Debug, Deserialize)]
struct LifecycleStateRow {
    lifecycle_state: RequestLifecycleState,
}
