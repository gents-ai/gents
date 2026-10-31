//! Per-request store budgets for a multi-turn tool-using workload.
//!
//! Every row a request leaves behind must be paid for by the content that
//! request added, never by the length of the history it replays: capture v4
//! shares content-defined manifests and reduction snapshots across turns
//! (#2865, #2888, #2538), reasoning deltas coalesce, and same-value writes
//! are dropped (#2537). A 0.21.0-rc3 node nevertheless reached 1.2 GB after
//! 401 AgentRequests (155,896 AgentOutputSegment and 108,352
//! RenderedRequestBlock rows), so these budgets fail the change that lets a
//! store outgrow its content again. The budgets are only meaningful while the
//! content survives, so the same run checks that provider input replays the
//! prior turns and that the transcript reconstructs what was streamed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::defra_node::EmbeddedNode;
use gents::inference::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents::{CompactionStrategy, Gents, NodeIdentity, ToolCeiling};
use serde_json::Value;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{
    create_runtime_request, wait_for_request_lifecycle_state, wait_for_runtime_ready, BootedAgent,
};
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{test_db, TestDb};

const MODEL: &str = "budget-model";
const BACKEND_ID: &str = "budget-backend";
const AGENT_ID: &str = "budget-agent";
const TOOL: &str = "audit_probe";

const SESSIONS: usize = 3;
const TURNS: usize = 6;
/// Longer than any scripted stream, so no flush is due by elapsed time and
/// every segment comes from a structural boundary (first visible output, a
/// tool call, a provider close). Segment counts are then exact on any
/// runner. Time-based cadence is covered on a simulated clock by
/// `streaming::canonical_tests` (#2923).
const STREAM_BATCH_MS: u64 = 600_000;
const REASONING_CHUNKS: usize = 160;
const TEXT_CHUNKS: usize = 120;
/// A small window: by the last turn of a session, provider input sits at the
/// reduction budget.
const CONTEXT_WINDOW: usize = 16_000;
const COMPACTION_THRESHOLD: f64 = 0.5;

/// Collections every request writes into, in report order.
const COLLECTIONS: [&str; 9] = [
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentOutputSegment",
    "InferenceCall",
    "RenderedRequest",
    "RenderedRequestBlock",
    "ProviderContextReduction",
    "AgentSession",
];

/// The 0.21 targets, each stated against the content a request adds.
///
/// Measured on main `2a1362fe3` (3 sessions x 6 turns, 2 inference calls and
/// 1 tool call per request; three runs, identical):
///
/// | Measure                                 | main           | budget  |
/// |-----------------------------------------|----------------|---------|
/// | turn 5 / turn 1 fresh block bytes       | 2.22x - 2.39x  | 1.50x   |
/// | AgentOutputSegment per request          | 8              | 8       |
/// | segments, 240 vs 24 chunks              | 3 vs 3         | equal   |
/// | flushed store bytes per AgentRequest    | ~155 KB        | 256 KiB |
///
/// Only the first row fails on main: the bytes a turn adds to
/// RenderedRequestBlock grow with the history it replays (turn 1 ~26 KB,
/// turn 4 ~33-47 KB, turn 5 ~58-66 KB once the window strips its own tool
/// result and the replayed prefix is captured again).
mod budget {
    /// EXPECTED TO FAIL ON MAIN `2a1362fe3` (2.22x - 2.39x). Fresh
    /// RenderedRequestBlock bytes a late turn may add, relative to the first
    /// follow-up turn of its session. Shared manifests make a turn pay for
    /// its new messages only, so the ratio stays near 1 however long the
    /// replayed history is.
    pub const LATE_TURN_BLOCK_BYTES_GROWTH: f64 = 1.5;
    /// AgentOutputSegment rows per request when only structural boundaries
    /// flush (first visible output, the tool call, each provider close, the
    /// authored input and the tool output). A per-delta writer would add ~360.
    pub const SEGMENTS_PER_REQUEST: usize = 8;
    /// Flushed on-disk bytes per AgentRequest, against the ~12 KB of new
    /// content each scripted request carries. The 0.21.0-rc3 node that
    /// prompted this test stored ~3 MB per request.
    pub const DISK_BYTES_PER_REQUEST: u64 = 256 * 1024;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_request_store_growth_follows_new_content_not_history() {
    let started = Instant::now();
    let backend = MockStreamingBackend::start_with_plans(MODEL, workload_plans()).unwrap();
    let mut db = test_db("storage-budget").await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(test_identity("storage-budget"));
    install_backend(&db.node, identity.did(), backend.endpoint()).await;
    db.simulate_process_crash().await.expect("flush baseline");
    let baseline_disk = dir_bytes(db.data_path());
    let agent = boot_agent(&db, identity.clone()).await;

    let mut samples = Vec::new();
    let mut before = Snapshot::take(&db).await;
    for session in 0..SESSIONS {
        for turn in 0..TURNS {
            let marker = turn_marker(session, turn);
            let doc_id = create_runtime_request(
                db.node.as_ref(),
                &agent.node_did,
                AGENT_ID,
                &format!("req-{marker}"),
                &session_id(session),
                &prompt(&marker),
            )
            .await;
            wait_for_request_lifecycle_state(db.node.as_ref(), &doc_id, "completed").await;
            let after = Snapshot::take(&db).await;
            let mut sample = Sample::between(session, turn, &before, &after);
            sample.history_bytes = backend
                .observed_completion_bodies()
                .last()
                .map_or(0, |body| body.to_string().len() as u64);
            samples.push(sample);
            before = after;
        }
    }

    let calibration = calibrate_segments(&db, &agent, &backend).await;
    let bodies = backend.observed_completion_bodies();
    let transcripts = load_transcripts(&db, &agent.node_did).await;

    agent.shutdown().await;
    db.simulate_process_crash().await.expect("flush workload");
    let final_snapshot = Snapshot::take(&db).await;
    let disk = dir_bytes(db.data_path()).saturating_sub(baseline_disk);
    let report = Report {
        samples,
        calibration,
        totals: final_snapshot.counts.clone(),
        disk,
        elapsed: started.elapsed(),
    };
    eprintln!("{report}");

    assert_provider_input_replays_history(&bodies);
    assert_transcripts_reconstruct_streams(&transcripts);
    report.assert_budgets();
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

/// The last line of a turn's tool result: present only if the result was
/// replayed in full.
fn tool_result_tail(marker: &str) -> String {
    format!("{marker} finding 63: unchanged invariant holds")
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
    for (marker, chunks) in calibration_streams() {
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

/// The same answer in 24 chunks and in 240.
fn calibration_streams() -> [(&'static str, usize); 2] {
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

async fn calibrate_segments(
    db: &TestDb,
    agent: &BootedAgent,
    backend: &MockStreamingBackend,
) -> Vec<(String, usize, usize)> {
    let mut rows = Vec::new();
    for (marker, chunks) in calibration_streams() {
        let before = count(db.node.as_ref(), "AgentOutputSegment").await;
        let doc_id = create_runtime_request(
            db.node.as_ref(),
            &agent.node_did,
            AGENT_ID,
            &format!("req-{marker}"),
            &format!("session-{marker}"),
            &format!("{marker}: answer without tools."),
        )
        .await;
        wait_for_request_lifecycle_state(db.node.as_ref(), &doc_id, "completed").await;
        assert_eq!(backend.observed_chunks(marker), chunks, "{marker}");
        let segments = count(db.node.as_ref(), "AgentOutputSegment").await - before;
        rows.push((marker.to_string(), chunks, segments));
    }
    rows
}

/// The text of a provider body's current authored user message.
fn current_user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|message| message["role"] == "user")
        .map(|message| match &message["content"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// Every request is posted twice: before its tool call and after it. Later
/// turns replay each earlier prompt and answer verbatim, and each earlier
/// tool result as a stub naming the call and its size. A request's own tool
/// result replays in full until the last turn, whose window strips it too.
fn assert_provider_input_replays_history(bodies: &[Value]) {
    for session in 0..SESSIONS {
        for turn in 0..TURNS {
            let marker = turn_marker(session, turn);
            let posted: Vec<String> = bodies
                .iter()
                .filter(|body| current_user_text(body).starts_with(&format!("{marker}:")))
                .map(Value::to_string)
                .collect();
            assert_eq!(
                posted.len(),
                2,
                "{marker}: one body before and one after its tool call"
            );
            let [before_tool, after_tool] = [&posted[0], &posted[1]];
            let own_in_full = after_tool.contains(&json_fragment(&tool_result(&marker)));
            if turn + 1 < TURNS {
                assert!(
                    own_in_full,
                    "{marker}: the follow-up call must carry this turn's own tool result in full"
                );
            } else {
                // The last turn reaches the window's reduction budget, which
                // strips the closed tool pair it just made.
                assert!(
                    !own_in_full && after_tool.contains(&json_fragment(&stripped_stub(&marker))),
                    "{marker}: the window must strip this turn's tool result to its stub"
                );
            }
            for prior in 0..turn {
                let earlier = turn_marker(session, prior);
                for body in [before_tool, after_tool] {
                    for (what, expected) in [
                        ("prompt", json_fragment(&prompt(&earlier))),
                        ("answer", json_fragment(&answer(&earlier).concat())),
                        ("tool result stub", json_fragment(&stripped_stub(&earlier))),
                    ] {
                        assert!(
                            body.contains(&expected),
                            "{marker}: provider input dropped the {what} of {earlier}"
                        );
                    }
                    assert!(
                        !body.contains(&tool_result_tail(&earlier)),
                        "{marker}: {earlier}'s tool result must replay as its stub, not in full"
                    );
                }
            }
        }
    }
}

/// What replaces a stripped tool result in provider input.
fn stripped_stub(marker: &str) -> String {
    let call = format!("call-{marker}");
    let bytes = tool_result(marker).len();
    format!(
        "[tool: {TOOL}, call_id: {call}, {bytes} bytes \u{2014} full output: sessions \
         {{\"action\":\"output\",\"call_id\":\"{call}\"}}]"
    )
}

/// The durable transcript keeps every streamed reasoning and answer byte, and
/// every tool result in full, whatever provider input stripped.
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

async fn load_transcripts(db: &TestDb, node_did: &str) -> Vec<String> {
    let mut transcripts = Vec::new();
    for session in 0..SESSIONS {
        let messages = gents::trace_export::load_trace_transcript(
            db.node.as_ref(),
            &session_id(session),
            node_did,
            Some(node_did),
            None,
            None,
            None,
        )
        .await
        .expect("load session transcript");
        let messages: Vec<_> = messages.iter().map(|row| &row.message).collect();
        transcripts.push(serde_json::to_string(&messages).expect("serialize transcript"));
    }
    transcripts
}

/// `text` as it appears inside a JSON string.
fn json_fragment(text: &str) -> String {
    let quoted = serde_json::to_string(text).expect("serialize string");
    quoted[1..quoted.len() - 1].to_string()
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    counts: BTreeMap<&'static str, usize>,
    /// RenderedRequestBlock rows by id. `byte_len` is deliberately unset in
    /// the schema, so block bytes are measured from the payloads themselves.
    blocks: BTreeMap<String, u64>,
    disk: u64,
}

impl Snapshot {
    async fn take(db: &TestDb) -> Self {
        let mut counts = BTreeMap::new();
        for collection in COLLECTIONS {
            counts.insert(collection, count(db.node.as_ref(), collection).await);
        }
        Self {
            counts,
            blocks: blocks(db.node.as_ref()).await,
            disk: dir_bytes(db.data_path()),
        }
    }
}

#[derive(Debug)]
struct Sample {
    session: usize,
    turn: usize,
    added: BTreeMap<&'static str, usize>,
    block_bytes: u64,
    /// Unflushed: memtable logs included, so this tracks bytes written.
    disk_written: u64,
    /// The request's last provider body: the history it replays.
    history_bytes: u64,
}

impl Sample {
    fn between(session: usize, turn: usize, before: &Snapshot, after: &Snapshot) -> Self {
        Self {
            session,
            turn,
            added: COLLECTIONS
                .iter()
                .map(|name| (*name, after.counts[name] - before.counts[name]))
                .collect(),
            block_bytes: after
                .blocks
                .iter()
                .filter(|(id, _)| !before.blocks.contains_key(*id))
                .map(|(_, bytes)| bytes)
                .sum(),
            disk_written: after.disk.saturating_sub(before.disk),
            history_bytes: 0,
        }
    }

    fn inference_calls(&self) -> usize {
        self.added["InferenceCall"].max(1)
    }
}

struct Report {
    samples: Vec<Sample>,
    calibration: Vec<(String, usize, usize)>,
    totals: BTreeMap<&'static str, usize>,
    disk: u64,
    elapsed: Duration,
}

impl Report {
    fn requests(&self) -> u64 {
        self.samples.len() as u64
    }

    /// Fresh block bytes of each session's last turn over its first
    /// follow-up turn (turn 0 pays for the system prompt and tool schemas).
    fn late_turn_block_growth(&self) -> Vec<(usize, f64)> {
        (0..SESSIONS)
            .map(|session| {
                let of = |turn| {
                    self.samples
                        .iter()
                        .find(|sample| sample.session == session && sample.turn == turn)
                        .map(|sample| sample.block_bytes.max(1) as f64)
                        .unwrap()
                };
                (session, of(TURNS - 1) / of(1))
            })
            .collect()
    }

    fn assert_budgets(&self) {
        let mut failures = Vec::new();

        // EXPECTED TO FAIL ON MAIN 2a1362fe3 (2.22x - 2.39x): a late turn
        // re-stores the history it replays.
        for (session, growth) in self.late_turn_block_growth() {
            if growth > budget::LATE_TURN_BLOCK_BYTES_GROWTH {
                failures.push(format!(
                    "session {session}: turn {} added {growth:.2}x the block bytes of turn 1 \
                     (budget {:.2}x): captures grow with history, not new content",
                    TURNS - 1,
                    budget::LATE_TURN_BLOCK_BYTES_GROWTH
                ));
            }
        }

        for sample in &self.samples {
            let segments = sample.added["AgentOutputSegment"];
            if segments > budget::SEGMENTS_PER_REQUEST {
                failures.push(format!(
                    "s{}t{}: {segments} AgentOutputSegment rows for one request (budget {})",
                    sample.session,
                    sample.turn,
                    budget::SEGMENTS_PER_REQUEST
                ));
            }
        }

        if let [(_, _, few), (_, _, many)] = self.calibration.as_slice() {
            if many != few {
                failures.push(format!(
                    "the same answer in 10x more chunks wrote {many} segments instead of {few}: \
                     segment count follows chunk count"
                ));
            }
        }

        let per_request = self.disk / self.requests();
        if per_request > budget::DISK_BYTES_PER_REQUEST {
            failures.push(format!(
                "{per_request} flushed bytes per AgentRequest (budget {})",
                budget::DISK_BYTES_PER_REQUEST
            ));
        }

        assert!(
            failures.is_empty(),
            "storage budgets exceeded:\n  {}",
            failures.join("\n  ")
        );
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "storage budget report ({:.1?})", self.elapsed)?;
        writeln!(
            f,
            "{:>6} {:>5} {:>5} {:>5} {:>5} {:>5} {:>5} {:>6} {:>9} {:>9} {:>9} {:>8}",
            "req",
            "msg",
            "tool",
            "seg",
            "call",
            "rr",
            "rrb",
            "reduce",
            "history",
            "rrb_bytes",
            "disk_wr",
            "seg/call"
        )?;
        for sample in &self.samples {
            let added = &sample.added;
            writeln!(
                f,
                "{:>6} {:>5} {:>5} {:>5} {:>5} {:>5} {:>5} {:>6} {:>9} {:>9} {:>9} {:>8.1}",
                format!("s{}t{}", sample.session, sample.turn),
                added["AgentMessage"],
                added["AgentToolCall"],
                added["AgentOutputSegment"],
                added["InferenceCall"],
                added["RenderedRequest"],
                added["RenderedRequestBlock"],
                added["ProviderContextReduction"],
                sample.history_bytes,
                sample.block_bytes,
                sample.disk_written,
                added["AgentOutputSegment"] as f64 / sample.inference_calls() as f64,
            )?;
        }
        for (session, growth) in self.late_turn_block_growth() {
            writeln!(
                f,
                "session {session}: turn {} / turn 1 block bytes = {growth:.2}x (budget {:.2}x)",
                TURNS - 1,
                budget::LATE_TURN_BLOCK_BYTES_GROWTH
            )?;
        }
        writeln!(
            f,
            "segment budget per request: {}",
            budget::SEGMENTS_PER_REQUEST
        )?;
        for (marker, chunks, segments) in &self.calibration {
            writeln!(f, "{marker}: {chunks} chunks -> {segments} segments")?;
        }
        writeln!(f, "totals: {:?}", self.totals)?;
        let requests = self.requests();
        let calls = self.totals["InferenceCall"].max(1) as u64;
        write!(
            f,
            "flushed store growth: {} bytes = {} per request, {} per inference call \
             (budget {} per request)",
            self.disk,
            self.disk / requests,
            self.disk / calls,
            budget::DISK_BYTES_PER_REQUEST
        )
    }
}

async fn count(node: &EmbeddedNode, collection: &str) -> usize {
    rows(node, collection, "_docID").await.len()
}

async fn blocks(node: &EmbeddedNode) -> BTreeMap<String, u64> {
    rows(node, "RenderedRequestBlock", "_docID payload")
        .await
        .iter()
        .filter_map(|row| {
            let id = row["_docID"].as_str()?.to_owned();
            let bytes = row["payload"].as_str().map_or(0, str::len) as u64;
            Some((id, bytes))
        })
        .collect()
}

async fn rows(node: &EmbeddedNode, collection: &str, fields: &str) -> Vec<Value> {
    let response = node
        .execute(&format!("query {{ {collection} {{ {fields} }} }}"))
        .await;
    assert!(
        !response.has_errors(),
        "{collection} query failed: {:?}",
        response.errors
    );
    response
        .data
        .and_then(|data| data.get(collection).cloned())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

fn dir_bytes(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| match entry.metadata() {
                    Ok(meta) if meta.is_dir() => dir_bytes(&entry.path()),
                    Ok(meta) => meta.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Returns a distinct ~4 KB audit finding per call, so tool output cannot
/// dedupe across turns.
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

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let note = serde_json::from_str::<Value>(&args)
                .ok()
                .and_then(|value| value["note"].as_str().map(ToOwned::to_owned))
                .unwrap_or_default();
            Ok(tool_result(&note))
        })
    }
}

async fn boot_agent(db: &TestDb, identity: Arc<dyn NodeIdentity>) -> BootedAgent {
    let agent = Gents::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_agent_id(AGENT_ID)
        .tool_ceiling(ToolCeiling::meta_only())
        .agent(AGENT_ID)
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
    let node_did = agent.node_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &node_did).await;
    BootedAgent::new(shutdown_tx, handle, node_did)
}

async fn install_backend(node: &Arc<EmbeddedNode>, owner: &str, endpoint: &str) {
    use gents::store::config_client::{
        ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    gents::ensure_node(node, owner).await.unwrap();
    let profile_id = format!("{AGENT_ID}:inference");
    let documents = [
        (
            gents::Collection::InferenceBackend,
            serde_json::json!({"node_did":owner,"backend_id":BACKEND_ID,"name":BACKEND_ID,
                "provider_kind":"OpenAiCompatible","openai_wire_api":"chat_completions",
                "endpoint":endpoint,"auth":{"kind":"unauthenticated"},
                "max_concurrent":4,"max_queue_depth":100}),
        ),
        (
            gents::Collection::InferenceProfile,
            serde_json::json!({"node_did":owner,"profile_id":profile_id,
                "backend_id":BACKEND_ID,"model_name":MODEL}),
        ),
        (
            gents::Collection::Agent,
            serde_json::json!({"node_did":owner,"agent_id":AGENT_ID,
                "inference_profile_id":profile_id}),
        ),
    ];
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    ConfigAccess::transact_local_write(node, None, "test.install_budget_backend", |txn| {
        let plan = &plan;
        Box::pin(
            async move { gents::store::config_client::apply_desired_state_plan(txn, plan).await },
        )
    })
    .await
    .unwrap();
    gents::inference::backends::registry::set_backend_probe_status(
        node, owner, BACKEND_ID, "healthy",
    )
    .await
    .unwrap();
}
