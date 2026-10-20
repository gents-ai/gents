//! Retrieval evals over a corpus large enough to train DiskANN (more than
//! 1024 chunks): relevance, latency, ACP exclusion, restart recovery and
//! resumable backfill. They use the deterministic embedding stub unless
//! `GENTS_RETRIEVAL_EVAL_EMBEDDING_URL` names an Ollama `/api` endpoint
//! serving bge-m3, which measures the real model. `make retrieval-evals`
//! runs them; `GENTS_RETRIEVAL_EVAL_REPORT` receives the metrics as JSON.

use std::sync::Arc;
use std::time::{Duration, Instant};

use defra_node::EmbeddedNode;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::indexer::{index_all_sessions, index_session, SessionKey, TRANSCRIPT_CHUNK};
use super::tests::{seed_message, seed_request, seed_session, EmbeddingStub};
use crate::config_client::ConfigAccess;
use crate::llm::tool::Tool;
use crate::toolset::session_history::SessionHistoryTool;

const TOPICS: &[(&str, [&str; 6], &str)] = &[
    (
        "database migration",
        [
            "schema",
            "migration",
            "rollback",
            "postgres",
            "column",
            "index",
        ],
        "orchid",
    ),
    (
        "kubernetes autoscaling",
        [
            "kubernetes",
            "autoscaler",
            "replicas",
            "cluster",
            "pods",
            "node",
        ],
        "basalt",
    ),
    (
        "billing invoices",
        [
            "invoice",
            "billing",
            "stripe",
            "refund",
            "subscription",
            "tax",
        ],
        "cobalt",
    ),
    (
        "login authentication",
        [
            "oauth",
            "token",
            "login",
            "password",
            "credential",
            "refresh",
        ],
        "dahlia",
    ),
    (
        "frontend accessibility",
        [
            "accessibility",
            "screenreader",
            "contrast",
            "aria",
            "keyboard",
            "focus",
        ],
        "ember",
    ),
    (
        "search ranking",
        [
            "ranking",
            "relevance",
            "query",
            "bm25",
            "embedding",
            "recall",
        ],
        "fjord",
    ),
    (
        "mobile release",
        [
            "ios",
            "android",
            "appstore",
            "build",
            "testflight",
            "release",
        ],
        "garnet",
    ),
    (
        "observability dashboards",
        [
            "tracing",
            "metrics",
            "dashboard",
            "alert",
            "latency",
            "grafana",
        ],
        "harbor",
    ),
    (
        "data pipeline",
        [
            "pipeline",
            "ingestion",
            "kafka",
            "batch",
            "etl",
            "warehouse",
        ],
        "indigo",
    ),
    (
        "security audit",
        [
            "vulnerability",
            "audit",
            "cve",
            "patch",
            "firewall",
            "encryption",
        ],
        "juniper",
    ),
    (
        "pricing experiment",
        [
            "pricing",
            "experiment",
            "discount",
            "conversion",
            "cohort",
            "plan",
        ],
        "kestrel",
    ),
    (
        "hiring pipeline",
        [
            "candidate",
            "interview",
            "hiring",
            "offer",
            "recruiter",
            "onboarding",
        ],
        "lantern",
    ),
    (
        "vendor contract",
        [
            "contract",
            "clause",
            "liability",
            "vendor",
            "signature",
            "compliance",
        ],
        "meadow",
    ),
    (
        "network routing",
        ["dns", "packet", "router", "bandwidth", "subnet", "vpn"],
        "nectar",
    ),
    (
        "model training",
        [
            "training",
            "gpu",
            "dataset",
            "epoch",
            "checkpoint",
            "gradient",
        ],
        "obsidian",
    ),
    (
        "documentation site",
        [
            "docs",
            "tutorial",
            "guide",
            "reference",
            "examples",
            "readme",
        ],
        "prairie",
    ),
];
const FILLER: [&str; 10] = [
    "team", "week", "update", "review", "followup", "meeting", "status", "issue", "work", "notes",
];
const MESSAGES_PER_SESSION: u32 = 70;
const NEEDLE: u32 = 37;
const DID: &str = "did:key:z6MkretrievalEvalNode";

fn session_id(topic: usize) -> String {
    format!("topic-{topic:02}")
}

fn message_text(topic: usize, sequence: u32) -> String {
    let (name, terms, code) = TOPICS[topic];
    if sequence == NEEDLE {
        return format!("For the {name} work the final decision code was {code}.");
    }
    let i = sequence as usize;
    format!(
        "{} {} {} {} {} item {i}",
        terms[i % 6],
        terms[(i + 1) % 6],
        terms[(i + 3) % 6],
        FILLER[i % 10],
        FILLER[(i * 7) % 10]
    )
}

async fn seed_corpus(node: &Arc<EmbeddedNode>) {
    for topic in 0..TOPICS.len() {
        let session = session_id(topic);
        seed_session(node, DID, &session, TOPICS[topic].0).await;
        let request = seed_request(node, DID, &session, &format!("{session}-r"), "completed").await;
        for sequence in 1..=MESSAGES_PER_SESSION {
            seed_message(
                node,
                DID,
                &session,
                &request,
                sequence,
                &message_text(topic, sequence),
            )
            .await;
        }
    }
}

async fn read(node: &Arc<EmbeddedNode>, query: String) -> Value {
    ConfigAccess::transact_local_readonly(node, None, "test.retrieval.eval", |txn| {
        let query = query.clone();
        Box::pin(async move { txn.execute(&query).await })
    })
    .await
    .unwrap()
}

/// Sum an execute-explain counter across the plan tree.
fn total(explain: &Value, counter: &str) -> u64 {
    match explain {
        Value::Object(map) => {
            map.get(counter).and_then(Value::as_u64).unwrap_or(0)
                + map.values().map(|value| total(value, counter)).sum::<u64>()
        }
        Value::Array(items) => items.iter().map(|item| total(item, counter)).sum(),
        _ => 0,
    }
}

fn names_vector_index(explain: &Value) -> bool {
    match explain {
        Value::Object(map) => {
            map.get("vectorIndex").is_some_and(Value::is_string)
                || map.values().any(names_vector_index)
        }
        Value::Array(items) => items.iter().any(names_vector_index),
        _ => false,
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

async fn search(tool: &SessionHistoryTool, query: &str, limit: usize) -> (Value, Duration) {
    let started = Instant::now();
    let output = Tool::call(
        tool,
        serde_json::from_value(
            json!({"action":"search","mode":"semantic","query":query,"limit":limit}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    (serde_json::from_str(&output).unwrap(), started.elapsed())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "retrieval eval over a trained DiskANN index; run with make retrieval-evals"]
async fn retrieval_eval_relevance_latency_restart_and_backfill() {
    let stub = EmbeddingStub::start().await;
    let real_model = std::env::var("GENTS_RETRIEVAL_EVAL_EMBEDDING_URL").ok();
    let url = real_model.clone().unwrap_or_else(|| stub.url.clone());
    let data = tempfile::tempdir().unwrap();
    let open = || async {
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_embedding_url(&url)
                .data_path(data.path())
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        node
    };

    let node = open().await;
    seed_corpus(&node).await;
    let started = Instant::now();
    let interrupted = 4;
    for topic in 0..interrupted {
        let key = SessionKey {
            node_did: DID.into(),
            session_id: session_id(topic),
        };
        while index_session(&node, &None, &key).await.unwrap().more {}
    }
    node.shutdown().await;
    drop(node);

    let node = open().await;
    let resumed = index_all_sessions(&node, &None, &CancellationToken::new())
        .await
        .unwrap();
    let indexing = started.elapsed();
    let total_messages = TOPICS.len() as u32 * MESSAGES_PER_SESSION;
    let states = read(
        &node,
        format!(
            "{{{} {{session_id watermark pending}}}}",
            super::indexer::TRANSCRIPT_INDEX_STATE
        ),
    )
    .await;
    assert_eq!(resumed.sessions, TOPICS.len());
    assert_eq!(
        resumed.indexed_messages as u32,
        (TOPICS.len() - interrupted) as u32 * MESSAGES_PER_SESSION,
        "the restarted backfill resumes, it does not repeat finished sessions: {states}"
    );
    let count = read(&node, format!("{{n: COUNT({TRANSCRIPT_CHUNK}: {{}})}}")).await;
    assert_eq!(count["data"]["n"], total_messages);

    let probe = super::embedding::embed_query(
        &node,
        TRANSCRIPT_CHUNK,
        super::indexer::CHUNK_VECTOR_FIELD,
        "schema migration rollback",
    )
    .await
    .unwrap();
    let explain = read(
        &node,
        format!(
            "query @explain(type: execute) {}",
            super::search::candidates_query(&probe, None, 40)
        ),
    )
    .await;
    assert!(
        names_vector_index(&explain) && total(&explain, "indexFetches") > 0,
        "semantic search must route through the DiskANN index: {explain:#}"
    );

    let tool = SessionHistoryTool::new(node.clone(), DID);
    let mut latencies = Vec::new();
    let (mut needle_top1, mut needle_reciprocal, mut topic_top5) = (0usize, 0.0f64, 0usize);
    for (topic, (name, terms, _)) in TOPICS.iter().enumerate() {
        let (needle, elapsed) = search(
            &tool,
            &format!("Which decision code did we settle on for the {name}?"),
            10,
        )
        .await;
        latencies.push(elapsed);
        let rank =
            needle["hits"].as_array().unwrap().iter().position(|hit| {
                hit["session_id"] == session_id(topic) && hit["sequence"] == NEEDLE
            });
        needle_top1 += usize::from(rank == Some(0));
        needle_reciprocal += rank.map_or(0.0, |rank| 1.0 / (rank as f64 + 1.0));

        let (topical, elapsed) =
            search(&tool, &format!("{} {} {}", terms[0], terms[1], terms[2]), 5).await;
        latencies.push(elapsed);
        topic_top5 += usize::from(
            topical["hits"]
                .as_array()
                .unwrap()
                .iter()
                .any(|hit| hit["session_id"] == session_id(topic)),
        );
    }
    latencies.sort();
    let queries = TOPICS.len() as f64;
    let report = json!({
        "embedding": if real_model.is_some() { "bge-m3" } else { "stub" },
        "chunks": total_messages,
        "indexing_seconds": indexing.as_secs_f64(),
        "needle_hit_at_1": needle_top1 as f64 / queries,
        "needle_mrr": needle_reciprocal / queries,
        "topic_recall_at_5": topic_top5 as f64 / queries,
        "latency_p50_ms": percentile(&latencies, 0.5).as_millis(),
        "latency_p95_ms": percentile(&latencies, 0.95).as_millis(),
        "latency_max_ms": latencies.last().unwrap().as_millis(),
    });
    tracing::info!(%report, "retrieval eval");
    if let Ok(path) = std::env::var("GENTS_RETRIEVAL_EVAL_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    let (min_needle, min_topic) = if real_model.is_some() {
        (0.6, 0.8)
    } else {
        (0.9, 0.95)
    };
    assert!(
        report["needle_hit_at_1"].as_f64().unwrap() >= min_needle,
        "{report}"
    );
    assert!(
        report["topic_recall_at_5"].as_f64().unwrap() >= min_topic,
        "{report}"
    );
    assert!(
        percentile(&latencies, 0.95) < Duration::from_secs(2),
        "semantic search latency regressed: {report}"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn semantic_hits_exclude_sessions_acp_hides_from_the_caller() {
    use crate::identity::{KeyIdentity, NodeIdentity};
    use crate::lifecycle::test_support::pin_fixed_signing_identity;

    let stub = EmbeddingStub::start().await;
    let identities = tempfile::tempdir().unwrap();
    let owner = pin_fixed_signing_identity(identities.path());
    let reader = KeyIdentity::load_or_create(identities.path().join("reader.key"), None).unwrap();
    let denied = KeyIdentity::load_or_create(identities.path().join("denied.key"), None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_embedding_url(&stub.url)
            .build()
            .await
            .unwrap(),
    );
    let policy = node
        .add_dac_policy(
            owner.did(),
            "name: Sessions reader eval\nresources:\n  - name: sessions\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
        )
        .await
        .unwrap();
    let access = ConfigAccess::local(node.clone());
    access
        .add_schema(&crate::schema::AGENT_SESSION_SCHEMA.replace(
            "type AgentSession ",
            &format!("type AgentSession @policy(id: \"{policy}\", resource: \"sessions\") "),
        ))
        .await
        .unwrap();
    for sdl in [
        gents_protocol::schemas::AGENT_REQUEST,
        gents_protocol::schemas::AGENT_MESSAGE,
        gents_protocol::schemas::AGENT_OUTPUT_SEGMENT,
        gents_protocol::schemas::TRANSCRIPT_CHUNK,
        gents_protocol::schemas::TRANSCRIPT_INDEX_STATE,
    ] {
        access.add_schema(sdl).await.unwrap();
    }
    let owner_did = owner.did().to_owned();
    let mut session_docs = Vec::new();
    for (session, text) in [
        (
            "shared",
            "The shared plan moves the release train to Thursday.",
        ),
        (
            "private",
            "The private plan moves the release train to Friday.",
        ),
    ] {
        let did = owner_did.clone();
        let doc = ConfigAccess::transact_local_write(&node, Some(owner_did.parse().unwrap()), "test.retrieval.acp.session", move |txn| {
            let did = did.clone();
            Box::pin(async move {
                let value = txn.execute(&format!("mutation {{add_AgentSession(input: {{node_did: \"{did}\", session_id: \"{session}\", agent_id: \"engineer\", created_at: \"2026-01-01T00:00:00Z\"}}) {{_docID}}}}")).await?;
                Ok(value["data"]["add_AgentSession"][0]["_docID"].as_str().unwrap().to_owned())
            })
        })
        .await
        .unwrap();
        session_docs.push(doc);
        let request = seed_request(
            &node,
            owner.did(),
            session,
            &format!("{session}-r"),
            "completed",
        )
        .await;
        seed_message(&node, owner.did(), session, &request, 1, text).await;
    }
    node.add_dac_actor_relationship(
        owner.did(),
        "AgentSession",
        &session_docs[0],
        "reader",
        reader.did(),
    )
    .await
    .unwrap();

    let pass = index_all_sessions(
        &node,
        &Some(owner_did.parse().unwrap()),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        pass.indexed_messages, 2,
        "the runtime DID indexes its own protected sessions"
    );

    let query = json!({"action":"search","mode":"semantic","query":"when does the release train move","limit":10});
    let owner_hits = Tool::call(
        &SessionHistoryTool::new(node.clone(), owner.did()),
        serde_json::from_value(query.clone()).unwrap(),
    )
    .await
    .unwrap();
    let owner_hits: Value = serde_json::from_str(&owner_hits).unwrap();
    assert_eq!(owner_hits["hits"].as_array().unwrap().len(), 2);
    let reader_hits = Tool::call(
        &SessionHistoryTool::new(node.clone(), reader.did()),
        serde_json::from_value(query.clone()).unwrap(),
    )
    .await
    .unwrap();
    let reader_hits: Value = serde_json::from_str(&reader_hits).unwrap();
    let sessions: Vec<_> = reader_hits["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["session_id"].clone())
        .collect();
    assert_eq!(sessions, vec![json!("shared")], "{reader_hits}");
    let denied_hits = Tool::call(
        &SessionHistoryTool::new(node.clone(), denied.did()),
        serde_json::from_value(query).unwrap(),
    )
    .await
    .unwrap();
    let denied_hits: Value = serde_json::from_str(&denied_hits).unwrap();
    assert!(
        denied_hits["hits"].as_array().unwrap().is_empty(),
        "{denied_hits}"
    );
    node.shutdown().await;
}
