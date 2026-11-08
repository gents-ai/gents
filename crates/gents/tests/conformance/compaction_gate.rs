use super::*;
use std::time::Duration;

use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::support::agent_requests::{canonical_now, RequestActors, RequestSeed};
use super::support::interrupt::{managed_runtime_builder, spawn_runtime, BootedAgent};
use super::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse,
};

const MODEL: &str = "default";
const BACKEND_ID: &str = "backend-compaction-gate";
const REQUEST_MARKER: &str = "compaction-gate-request";
const REUSED_CALL_ID: &str = "provider-call-reused-across-turns";
const SEEDED_TURNS: usize = 12;
const SEEDED_TURN_BYTES: usize = 10_000;

pub(super) async fn compaction_runtime_reduces_valid_canonical_history() {
    let db = test_db("compaction-gate").await;
    let compaction_marker = gents_loop::compaction::summary::compaction_request_prompt();
    let checkpoint =
        serde_json::to_string(&gents_loop::compaction::summary::ContinuationCheckpoint {
            goal: "continue".into(),
            completed_work: vec!["canonical history summarized".into()],
            next_actions: vec!["finish the current request".into()],
            ..Default::default()
        })
        .expect("serialize canonical checkpoint");
    let backend = MockStreamingBackend::start_with_plans(
        MODEL,
        vec![
            StreamPlan::current_authored_user(
                compaction_marker,
                vec![StreamResponse::streams(
                    compaction_marker,
                    vec![StreamChunk::text(checkpoint)],
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
    let actors = RequestActors::new(&db).await;
    let agent = boot_agent(&db, &actors, backend.endpoint()).await;
    let session_id = format!("session-{}", uuid::Uuid::new_v4());
    seed_bulky_history(&actors, &session_id).await;

    let first = run_request(&actors, &session_id, "first").await;
    assert_eq!(first.lifecycle_state, RequestLifecycleState::Completed);
    assert!(backend.observed_requests(compaction_marker) >= 1);
    assert_eq!(
        tool_call_count(&actors, &session_id, REUSED_CALL_ID).await,
        2,
        "the same provider call id is valid in two distinct request-local turns"
    );
    let first_entries =
        support::snapshots::fetch_compaction_entry_snapshots_for_session(&actors, &session_id)
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

    let second = run_request(&actors, &session_id, "second").await;
    assert_eq!(second.lifecycle_state, RequestLifecycleState::Completed);
    assert!(backend.observed_requests(REQUEST_MARKER) >= 2);
    assert!(
        support::snapshots::fetch_compaction_entry_snapshots_for_session(&actors, &session_id)
            .await
            .len()
            >= first_entries.len()
    );

    agent.shutdown().await;
}

async fn tool_call_count(actors: &RequestActors, session_id: &str, tool_call_id: &str) -> usize {
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
    let tool_call_id = gents::store::graphql::escape_graphql_string(tool_call_id);
    let response = actors
        .query(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, tool_call_id: {{ _eq: "{tool_call_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    gents::store::graphql::rows::<serde_json::Value>(&response, "AgentToolCall")
        .expect("AgentToolCall rows")
        .len()
}

async fn run_request(actors: &RequestActors, session_id: &str, suffix: &str) -> LifecycleStateRow {
    let request_id = format!("{suffix}-{}", uuid::Uuid::new_v4());
    let created_at = canonical_now();
    let mut seed = RequestSeed::new(&request_id, session_id, &created_at);
    seed.content = Some(REQUEST_MARKER);
    let doc_id = actors.create_request(seed).await;
    wait_for_terminal_request(actors, &doc_id).await
}

/// Earlier turns of the Agent's own session, enough to cross the threshold.
async fn seed_bulky_history(actors: &RequestActors, session_id: &str) {
    use gents_protocol::output::MessageRole;
    actors.session(session_id, &canonical_now()).await;
    actors
        .set_session_title(
            session_id,
            &gents_protocol::session::SessionTitle {
                text: "compaction gate".into(),
                source: gents_protocol::session::SessionTitleSource::Generated,
            },
        )
        .await;
    let mut sequence = 0u32;
    for turn in 0..SEEDED_TURNS {
        let payload = "h".repeat(SEEDED_TURN_BYTES);
        for (role, content) in [
            (MessageRole::User, format!("turn {turn}: {payload}")),
            (MessageRole::Assistant, format!("reply {turn}: {payload}")),
        ] {
            actors
                .publish_history_message(session_id, sequence, role, &content, &canonical_now())
                .await;
            sequence += 1;
        }
    }
}

async fn boot_agent(db: &support::TestDb, actors: &RequestActors, endpoint: &str) -> BootedAgent {
    support::fixtures::bind_managed_agent_backend(db, &actors.fixture, BACKEND_ID, endpoint, MODEL)
        .await;
    let runtime = managed_runtime_builder(db, actors.agent_did())
        .agent(actors.agent_did())
        .identity_binding(actors.fixture.binding.clone())
        .backend_id(BACKEND_ID)
        .model_name(MODEL)
        .stream_batch_ms(0)
        .context_window(30_000)
        .compaction_threshold(0.5)
        .done()
        .build()
        .await
        .expect("build compaction agent");
    spawn_runtime(db, runtime).await
}

async fn wait_for_terminal_request(
    actors: &RequestActors,
    request_doc_id: &str,
) -> LifecycleStateRow {
    let doc_id = gents::store::graphql::escape_graphql_string(request_doc_id);
    let started = std::time::Instant::now();
    loop {
        let response = actors
            .query(&format!(
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
