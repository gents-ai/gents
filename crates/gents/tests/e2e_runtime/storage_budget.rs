//! Per-request store budgets for a multi-turn tool-using workload.
//!
//! Every row a request leaves behind must be paid for by the content that
//! request added, never by the length of the history it replays: capture v4
//! shares content-defined manifests and reduction snapshots across turns
//! (#2865, #2888, #2538), reasoning deltas coalesce, and same-value writes
//! are dropped (#2537). A 0.21.0-rc3 node nevertheless reached 1.2 GB after
//! 401 AgentRequests (155,896 AgentOutputSegment and 108,352
//! RenderedRequestBlock rows), so these budgets fail the change that lets a
//! store outgrow its content again.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::defra_node::EmbeddedNode;
use gents::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
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
/// Persistence batching for the workload. Production defaults to 1000 ms over
/// streams that last seconds; the ratio of stream time to batch interval is
/// what bounds segments, so the scripted streams last a few batch intervals.
const STREAM_BATCH_MS: u64 = 40;
const CHUNK_DELAY: Duration = Duration::from_millis(1);
const REASONING_CHUNKS: usize = 160;
const TEXT_CHUNKS: usize = 120;
/// A small window so the later turns of each session reduce provider context.
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
/// 1 tool call per request; three runs, identical where not noted):
///
/// | Measure                                 | main           | budget  |
/// |-----------------------------------------|----------------|---------|
/// | turn 5 / turn 1 fresh block bytes       | 2.22x - 2.39x  | 1.50x   |
/// | AgentOutputSegment per request          | 13 - 18        | 24      |
/// | segments, 240 vs 24 chunks, same time   | 7-8 vs 6-7     | 2x + 2  |
/// | flushed store bytes per AgentRequest    | ~169 KB        | 256 KiB |
///
/// Only the first row fails on main: the bytes a turn adds to
/// RenderedRequestBlock grow with the history it replays (turn 1 ~26 KB,
/// turn 4 ~33-47 KB, turn 5 ~58-66 KB once tool results are stripped).
mod budget {
    /// EXPECTED TO FAIL ON MAIN `2a1362fe3` (2.22x - 2.39x). Fresh
    /// RenderedRequestBlock bytes a late turn may add, relative to the first
    /// follow-up turn of its session. Shared manifests make a turn pay for
    /// its new messages only, so the ratio stays near 1 however long the
    /// replayed history is.
    pub const LATE_TURN_BLOCK_BYTES_GROWTH: f64 = 1.5;
    /// AgentOutputSegment rows per request. Two streams of ~180 ms at 40 ms
    /// batches flush about 10 times; persisting every delta would write ~360.
    /// The margin absorbs CI timer jitter, not per-delta writes.
    pub const SEGMENTS_PER_REQUEST: usize = 24;
    /// A stream split into 10x more chunks over the same wall time may write
    /// at most `2 * few + 2` segments: chunk count must not drive persistence.
    pub const CHUNK_COUNT_SEGMENT_FACTOR: usize = 2;
    pub const CHUNK_COUNT_SEGMENT_SLACK: usize = 2;
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
                &format!("session-budget-{session}"),
                &format!("{marker}: audit the next module and report what changed."),
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
    report.assert_budgets();
}

fn turn_marker(session: usize, turn: usize) -> String {
    format!("budget-s{session}-t{turn}")
}

/// Each request: a reasoning stream that ends in one tool call, then a
/// reasoning-and-text stream that answers from the tool output.
fn workload_plans() -> Vec<StreamPlan> {
    let mut plans = Vec::new();
    for session in 0..SESSIONS {
        for turn in 0..TURNS {
            let marker = turn_marker(session, turn);
            let mut first = reasoning_chunks(&marker, REASONING_CHUNKS);
            first.push(StreamChunk::tool_call(
                format!("call-{marker}"),
                TOOL,
                format!(r#"{{"note":"{marker}"}}"#),
            ));
            let mut second = reasoning_chunks(&marker, REASONING_CHUNKS / 2);
            second.extend(text_chunks(&marker, TEXT_CHUNKS, 20));
            plans.push(StreamPlan::current_authored_user(
                marker.clone(),
                vec![
                    StreamResponse::Stream(
                        StreamScript::streams(marker.clone(), first).with_chunk_delay(CHUNK_DELAY),
                    ),
                    StreamResponse::Stream(
                        StreamScript::streams(marker.clone(), second).with_chunk_delay(CHUNK_DELAY),
                    ),
                ],
            ));
        }
    }
    for (marker, chunks, delay) in calibration_streams() {
        plans.push(StreamPlan::current_authored_user(
            marker,
            vec![StreamResponse::Stream(
                StreamScript::streams(marker, text_chunks(marker, chunks, 4_800 / chunks))
                    .with_chunk_delay(delay),
            )],
        ));
    }
    plans
}

/// Two text streams with the same bytes over the same wall time, one in 10x
/// more chunks.
fn calibration_streams() -> [(&'static str, usize, Duration); 2] {
    [
        ("budget-few-chunks", 24, Duration::from_millis(10)),
        ("budget-many-chunks", 240, Duration::from_millis(1)),
    ]
}

fn reasoning_chunks(marker: &str, count: usize) -> Vec<StreamChunk> {
    (0..count)
        .map(|index| StreamChunk::reasoning(format!("[{marker} r{index:03}] weigh ")))
        .collect()
}

fn text_chunks(marker: &str, count: usize, width: usize) -> Vec<StreamChunk> {
    (0..count)
        .map(|index| {
            let word = format!("{marker}:{index:03} ");
            StreamChunk::text(word.repeat(width.div_ceil(word.len()).max(1)))
        })
        .collect()
}

async fn calibrate_segments(
    db: &TestDb,
    agent: &BootedAgent,
    backend: &MockStreamingBackend,
) -> Vec<(String, usize, usize)> {
    let mut rows = Vec::new();
    for (marker, chunks, _) in calibration_streams() {
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

        let ceiling = budget::SEGMENTS_PER_REQUEST;
        for sample in &self.samples {
            let segments = sample.added["AgentOutputSegment"];
            if segments > ceiling {
                failures.push(format!(
                    "s{}t{}: {segments} AgentOutputSegment rows for one request \
                     (budget {ceiling} at {STREAM_BATCH_MS} ms batches)",
                    sample.session, sample.turn
                ));
            }
        }

        if let [(_, _, few), (_, _, many)] = self.calibration.as_slice() {
            if *many > few * budget::CHUNK_COUNT_SEGMENT_FACTOR + budget::CHUNK_COUNT_SEGMENT_SLACK
            {
                failures.push(format!(
                    "10x more chunks over the same wall time wrote {many} segments \
                     instead of {few}: segment count follows chunk count"
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
            Ok((0..64)
                .map(|line| format!("{note} finding {line:02}: unchanged invariant holds\n"))
                .collect())
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
    use gents::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
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
        Box::pin(async move { gents::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
    gents::backend_registry::set_backend_probe_status(node, owner, BACKEND_ID, "healthy")
        .await
        .unwrap();
}
