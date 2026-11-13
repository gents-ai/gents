use std::path::Path;
use std::time::Instant;

use gents_inference::compaction::CompactionStrategy;
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents_tools::surface::ToolCeiling;
use serde_json::Value;

use crate::support::agent_requests::RequestActors;
use crate::support::builder_runtime::{
    builder_actors, fixture_agent, runtime_builder, spawn_runtime, submit_request,
};
use crate::support::interrupt::BootedAgent;
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{test_db, ManagedAgentFixture, TestDb};

const MODEL: &str = "budget-model";
const BACKEND_ID: &str = "budget-backend";
const TOOL: &str = "audit_probe";
const SESSIONS: usize = 3;
const TURNS: usize = 6;
const STREAM_BATCH_MS: u64 = 600_000;
const REASONING_CHUNKS: usize = 160;
const TEXT_CHUNKS: usize = 120;
const CONTEXT_WINDOW: usize = 16_000;
const COMPACTION_THRESHOLD: f64 = 0.5;
const DISK_BYTES_PER_REQUEST: u64 = 256 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_request_store_growth_follows_new_content_not_history() {
    let started = Instant::now();
    let backend = MockStreamingBackend::start_with_plans(MODEL, workload_plans()).unwrap();
    let mut db = test_db("storage-budget").await;
    let fixture = into_fixture(builder_actors(&db, BACKEND_ID, backend.endpoint(), MODEL, 4).await);
    db.simulate_process_crash().await.expect("flush baseline");
    let actors = reopen_actors(&db, fixture).await;
    let baseline_disk = disk_bytes(db.data_path()).await;
    let agent = boot_agent(&db, &actors).await;
    let node_did = agent.node_did.clone();

    for session in 0..SESSIONS {
        for turn in 0..TURNS {
            let marker = turn_marker(session, turn);
            let request_id = format!("req-{marker}");
            submit_request(
                &actors,
                &request_id,
                &session_id(session),
                "interactive",
                &prompt(&marker),
            )
            .await;
            assert_eq!(
                wait_for_request_terminal(
                    &actors.agent_access,
                    &request_id,
                    std::time::Duration::from_secs(60),
                )
                .await,
                "completed",
            );
        }
    }
    for (marker, _) in output_only_streams() {
        let request_id = format!("req-{marker}");
        submit_request(
            &actors,
            &request_id,
            &format!("session-{marker}"),
            "interactive",
            &format!("{marker}: answer without tools."),
        )
        .await;
        assert_eq!(
            wait_for_request_terminal(
                &actors.agent_access,
                &request_id,
                std::time::Duration::from_secs(60),
            )
            .await,
            "completed",
        );
    }
    agent.shutdown().await;
    let fixture = into_fixture(actors);
    db.simulate_process_crash().await.expect("flush workload");
    let disk = disk_bytes(db.data_path())
        .await
        .saturating_sub(baseline_disk);
    let actors = reopen_actors(&db, fixture).await;
    let mut transcripts = Vec::new();
    for session in 0..SESSIONS {
        transcripts.push(load_transcript(&actors, &node_did, &session_id(session)).await);
    }
    assert_transcripts_reconstruct_streams(&transcripts);
    for (marker, chunks) in output_only_streams() {
        let transcript = load_transcript(&actors, &node_did, &format!("session-{marker}")).await;
        let answer = text_fragments(marker, chunks, 4_800 / chunks).concat();
        assert!(
            transcript.contains(&json_fragment(&answer)),
            "{marker}: durable answer is incomplete"
        );
    }
    let requests = (SESSIONS * TURNS) as u64;
    tracing::info!(disk_bytes = disk, requests, elapsed = ?started.elapsed(), "flushed fixed-workload storage");
    assert!(
        disk <= DISK_BYTES_PER_REQUEST * requests,
        "{disk} flushed bytes exceed {} for {requests} requests",
        DISK_BYTES_PER_REQUEST * requests,
    );
}

fn turn_marker(session: usize, turn: usize) -> String {
    format!("budget-s{session}-t{turn}")
}

fn session_id(session: usize) -> String {
    format!("session-budget-{session}")
}

fn prompt(marker: &str) -> String {
    format!("{marker}: audit the next module and report what changed.")
}

/// The first stream of a request: reasoning that ends in one tool call.
fn first_reasoning(marker: &str) -> Vec<String> {
    reasoning_fragments(marker, "a", REASONING_CHUNKS)
}

/// The second stream: more reasoning, then the answer.
fn second_reasoning(marker: &str) -> Vec<String> {
    reasoning_fragments(marker, "b", REASONING_CHUNKS / 2)
}

fn answer(marker: &str) -> Vec<String> {
    text_fragments(marker, TEXT_CHUNKS, 20)
}

fn tool_result(note: &str) -> String {
    (0..64)
        .map(|line| format!("{note} finding {line:02}: unchanged invariant holds\n"))
        .collect()
}

fn workload_plans() -> Vec<StreamPlan> {
    let mut plans = Vec::new();
    for session in 0..SESSIONS {
        for turn in 0..TURNS {
            let marker = turn_marker(session, turn);
            let mut first: Vec<_> = first_reasoning(&marker)
                .into_iter()
                .map(StreamChunk::reasoning)
                .collect();
            first.push(StreamChunk::tool_call(
                format!("call-{marker}"),
                TOOL,
                format!(r#"{{"note":"{marker}"}}"#),
            ));
            let second = second_reasoning(&marker)
                .into_iter()
                .map(StreamChunk::reasoning)
                .chain(answer(&marker).into_iter().map(StreamChunk::text))
                .collect();
            plans.push(StreamPlan::current_authored_user(
                marker.clone(),
                vec![
                    StreamResponse::streams(marker.clone(), first),
                    StreamResponse::streams(marker.clone(), second),
                ],
            ));
        }
    }
    for (marker, chunks) in output_only_streams() {
        plans.push(StreamPlan::current_authored_user(
            marker,
            vec![StreamResponse::Stream(StreamScript::streams(
                marker,
                text_fragments(marker, chunks, 4_800 / chunks)
                    .into_iter()
                    .map(StreamChunk::text)
                    .collect(),
            ))],
        ));
    }
    plans
}

fn output_only_streams() -> [(&'static str, usize); 2] {
    [("budget-few-chunks", 24), ("budget-many-chunks", 240)]
}

fn reasoning_fragments(marker: &str, stream: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("[{marker} {stream}{index:03}] weigh "))
        .collect()
}

fn text_fragments(marker: &str, count: usize, width: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            let word = format!("{marker}:{index:03} ");
            word.repeat(width.div_ceil(word.len()).max(1))
        })
        .collect()
}

fn assert_transcripts_reconstruct_streams(transcripts: &[String]) {
    for (session, transcript) in transcripts.iter().enumerate() {
        for turn in 0..TURNS {
            let marker = turn_marker(session, turn);
            for (what, expected) in [
                ("first reasoning", first_reasoning(&marker).concat()),
                ("second reasoning", second_reasoning(&marker).concat()),
                ("answer", answer(&marker).concat()),
                ("tool result", tool_result(&marker)),
            ] {
                assert!(
                    transcript.contains(&json_fragment(&expected)),
                    "{marker}: the transcript does not reconstruct the streamed {what}"
                );
            }
        }
    }
}

async fn load_transcript(actors: &RequestActors, node_did: &str, session: &str) -> String {
    let messages = gents_observe::observe::trace_export::load_trace_transcript(
        &actors.agent_access,
        session,
        node_did,
        Some(actors.agent_did()),
        None,
        None,
        None,
    )
    .await
    .expect("load durable session transcript");
    let messages: Vec<_> = messages.iter().map(|row| &row.message).collect();
    serde_json::to_string(&messages).expect("serialize transcript")
}

fn json_fragment(text: &str) -> String {
    let quoted = serde_json::to_string(text).expect("serialize string");
    quoted[1..quoted.len() - 1].to_string()
}

async fn disk_bytes(path: &Path) -> u64 {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || dir_bytes(&path))
        .await
        .expect("measure the persisted store")
        .expect("read the persisted store files")
}

fn dir_bytes(path: &Path) -> std::io::Result<u64> {
    std::fs::read_dir(path)?.try_fold(0, |total, entry| {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let bytes = if metadata.is_dir() {
            dir_bytes(&entry.path())?
        } else {
            metadata.len()
        };
        Ok(total + bytes)
    })
}

#[derive(Clone)]
struct AuditProbe;

impl ToolDyn for AuditProbe {
    fn name(&self) -> String {
        TOOL.to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: TOOL.to_string(),
                description: "Audits one module and reports its findings".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "note": { "type": "string" } },
                    "required": ["note"]
                }),
            }
        })
    }

    fn call<'a>(
        &'a self,
        args: String,
        _context: &'a gents_loop::tool::ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let note = serde_json::from_str::<Value>(&args)
                .ok()
                .and_then(|value| value["note"].as_str().map(ToOwned::to_owned))
                .unwrap_or_default();
            Ok(tool_result(&note))
        })
    }
}

async fn boot_agent(db: &TestDb, actors: &RequestActors) -> BootedAgent {
    let runtime = fixture_agent(
        runtime_builder(db, actors).tool_ceiling(ToolCeiling::meta_only()),
        actors,
        "budget-agent",
    )
    .backend_id(BACKEND_ID)
    .model_name(MODEL)
    .stream_batch_ms(STREAM_BATCH_MS)
    .deadline_duration_secs(60)
    .context_window(CONTEXT_WINDOW)
    .compaction_threshold(COMPACTION_THRESHOLD)
    .compaction_strategy(CompactionStrategy::StripToolResults)
    .custom_tool(AuditProbe)
    .done()
    .build()
    .await
    .expect("build storage budget agent");
    spawn_runtime(db, runtime).await
}

/// Releases the actors' handles on the current process, keeping the User and
/// Agent identities for the next one.
fn into_fixture(actors: RequestActors) -> ManagedAgentFixture {
    actors.fixture
}

/// The User and Agent keep their identities across a process restart; only
/// their accesses bind the reopened database.
async fn reopen_actors(db: &TestDb, mut fixture: ManagedAgentFixture) -> RequestActors {
    fixture.access = db.client_access(&fixture.user);
    RequestActors::from_fixture(db, fixture).await
}
