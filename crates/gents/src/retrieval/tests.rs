use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use axum::routing::post;
use axum::{Json, Router};
use defra_node::EmbeddedNode;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::indexer::{
    index_all_sessions, index_session, SessionKey, TRANSCRIPT_CHUNK, TRANSCRIPT_INDEX_STATE,
};
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::llm::tool::Tool;
use crate::toolset::session_history::SessionHistoryTool;

pub(crate) const DIMENSIONS: usize = 1024;

/// Deterministic Ollama-shaped embedding endpoint: bag-of-words hashed into
/// the transcript index's 1024 dimensions, so passages sharing content words
/// are near and unrelated ones are orthogonal.
pub(crate) struct EmbeddingStub {
    pub(crate) url: String,
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) failing: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for EmbeddingStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) fn embed_words(text: &str) -> Vec<f64> {
    const STOP: &[&str] = &[
        "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "we", "is", "was", "it",
        "that", "this", "with", "be", "are", "by", "as", "at", "from", "why", "what", "how", "did",
        "do",
    ];
    let mut vector = vec![0.0; DIMENSIONS];
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() > 1 && !STOP.contains(&word.as_str()))
    {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in word.as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        vector[(hash % DIMENSIONS as u64) as usize] += 1.0;
    }
    if vector.iter().all(|value| *value == 0.0) {
        vector[0] = 1.0;
    }
    vector
}

impl EmbeddingStub {
    pub(crate) async fn start() -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let failing = Arc::new(AtomicBool::new(false));
        let (route_calls, route_failing) = (calls.clone(), failing.clone());
        let app = Router::new().route(
            "/api/embeddings",
            post(move |Json(body): Json<Value>| {
                let calls = route_calls.clone();
                let failing = route_failing.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if failing.load(Ordering::SeqCst) {
                        return Err(axum::http::StatusCode::BAD_REQUEST);
                    }
                    Ok(Json(json!({
                        "embedding": embed_words(body["prompt"].as_str().unwrap_or_default())
                    })))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let url = format!("http://{}/api", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url,
            calls,
            failing,
            task,
        }
    }
}

pub(crate) async fn node_with(
    stub: &EmbeddingStub,
    data: Option<&std::path::Path>,
) -> Arc<EmbeddedNode> {
    let mut builder = EmbeddedNode::builder().with_embedding_url(&stub.url);
    if let Some(data) = data {
        builder = builder
            .data_path(data)
            .with_storage_backend(defra_node::StorageBackend::Regolith);
    }
    let node = Arc::new(builder.build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", escape_graphql_string(value))
}

pub(crate) async fn seed_session(node: &Arc<EmbeddedNode>, did: &str, session: &str, title: &str) {
    ConfigAccess::write_local(
        node,
        "test.retrieval.session",
        &format!(
            r#"mutation {{create_AgentSession(input: {{node_did: {}, session_id: {}, agent_id: "engineer", created_at: {}, title: {{text: {}, source: "user"}}}}) {{_docID}}}}"#,
            quoted(did),
            quoted(session),
            quoted(&chrono::Utc::now().to_rfc3339()),
            quoted(title)
        ),
    )
    .await
    .unwrap();
}

pub(crate) async fn seed_request(
    node: &Arc<EmbeddedNode>,
    did: &str,
    session: &str,
    id: &str,
    state: &str,
) -> String {
    let value = ConfigAccess::write_local(
        node,
        "test.retrieval.request",
        &format!(
            r#"mutation {{create_AgentRequest(input: {{request_id: {id}, retry_key: {id}, node_did: {}, session_id: {}, lifecycle_state: {}, created_at: {}}}) {{_docID}}}}"#,
            quoted(did),
            quoted(session),
            quoted(state),
            quoted(&chrono::Utc::now().to_rfc3339()),
            id = quoted(id),
        ),
    )
    .await
    .unwrap();
    let created = &value["data"]["add_AgentRequest"];
    created
        .get(0)
        .unwrap_or(created)
        .get("_docID")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("request fixture: {value}"))
        .to_owned()
}

pub(crate) async fn set_request_state(node: &Arc<EmbeddedNode>, doc_id: &str, state: &str) {
    ConfigAccess::write_local(
        node,
        "test.retrieval.request_state",
        &format!(
            r#"mutation {{update_AgentRequest(docID: {}, input: {{lifecycle_state: {}}}) {{_docID}}}}"#,
            quoted(doc_id),
            quoted(state)
        ),
    )
    .await
    .unwrap();
}

pub(crate) async fn seed_message(
    node: &Arc<EmbeddedNode>,
    did: &str,
    session: &str,
    request_doc_id: &str,
    sequence: u32,
    text: &str,
) {
    crate::session::import_history_observation(
        node,
        request_doc_id,
        session,
        did,
        None,
        text,
        &format!("{session}-{sequence}"),
        sequence,
        None,
    )
    .await;
}

async fn read(node: &Arc<EmbeddedNode>, query: &str) -> Value {
    let query = query.to_owned();
    ConfigAccess::transact_local_readonly(node, None, "test.retrieval.read", |txn| {
        let query = query.clone();
        Box::pin(async move { txn.execute(&query).await })
    })
    .await
    .unwrap()
}

async fn chunk_sequences(node: &Arc<EmbeddedNode>, session: &str) -> Vec<u64> {
    let value = read(
        node,
        &format!(
            "{{{TRANSCRIPT_CHUNK}(filter: {{session_id: {{_eq: {}}}}}, order: {{sequence: ASC}}) {{sequence ordinal}}}}",
            quoted(session)
        ),
    )
    .await;
    let mut sequences: Vec<u64> = value["data"][TRANSCRIPT_CHUNK]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["sequence"].as_u64().unwrap())
        .collect();
    sequences.dedup();
    sequences
}

async fn state(node: &Arc<EmbeddedNode>, session: &str) -> Option<(u64, u64)> {
    let value = read(
        node,
        &format!(
            "{{{TRANSCRIPT_INDEX_STATE}(filter: {{session_id: {{_eq: {}}}}}) {{watermark pending}}}}",
            quoted(session)
        ),
    )
    .await;
    value["data"][TRANSCRIPT_INDEX_STATE]
        .as_array()
        .unwrap()
        .first()
        .map(|row| {
            (
                row["watermark"].as_u64().unwrap(),
                row["pending"].as_u64().unwrap(),
            )
        })
}

fn key(did: &str, session: &str) -> SessionKey {
    SessionKey {
        node_did: did.into(),
        session_id: session.into(),
    }
}

async fn call(tool: &SessionHistoryTool, params: Value) -> Value {
    serde_json::from_str(
        &Tool::call(tool, serde_json::from_value(params).unwrap())
            .await
            .unwrap(),
    )
    .unwrap()
}

const DID: &str = "did:key:z6MkretrievalIndexTestNode";

#[tokio::test]
async fn settled_messages_are_indexed_and_in_flight_responses_wait() {
    let stub = EmbeddingStub::start().await;
    let node = node_with(&stub, None).await;
    seed_session(&node, DID, "s1", "Release").await;
    let done = seed_request(&node, DID, "s1", "r1", "completed").await;
    let running = seed_request(&node, DID, "s1", "r2", "processing").await;
    seed_message(&node, DID, "s1", &done, 1, "We chose blue green deploys.").await;
    seed_message(
        &node,
        DID,
        "s1",
        &done,
        2,
        "Rollback uses the previous slot.",
    )
    .await;
    seed_message(&node, DID, "s1", &running, 3, "Still streaming this answer").await;

    let pass = index_all_sessions(&node, &None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(pass.indexed_messages, 2);
    assert_eq!(chunk_sequences(&node, "s1").await, vec![1, 2]);
    assert_eq!(state(&node, "s1").await, Some((3, 1)));

    set_request_state(&node, &running, "completed").await;
    let pass = index_session(&node, &None, &key(DID, "s1")).await.unwrap();
    assert_eq!((pass.indexed_messages, pass.pending), (1, 0));
    assert_eq!(chunk_sequences(&node, "s1").await, vec![1, 2, 3]);
    assert_eq!(state(&node, "s1").await, Some((4, 0)));

    let calls = stub.calls.load(Ordering::SeqCst);
    let idle = index_session(&node, &None, &key(DID, "s1")).await.unwrap();
    assert_eq!((idle.indexed_messages, idle.deleted_messages), (0, 0));
    assert_eq!(
        stub.calls.load(Ordering::SeqCst),
        calls,
        "an idle pass embeds nothing"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn semantic_search_ranks_by_meaning_and_returns_canonical_references() {
    let stub = EmbeddingStub::start().await;
    let node = node_with(&stub, None).await;
    for (session, texts) in [
        (
            "infra",
            [
                "The database migration failed so we rolled back the schema change.",
                "Kubernetes autoscaling thresholds were raised to eighty percent.",
            ],
        ),
        (
            "frontend",
            [
                "The button colour palette moved to the new design tokens.",
                "Accessibility audit flagged missing alt text on images.",
            ],
        ),
    ] {
        seed_session(&node, DID, session, session).await;
        let request = seed_request(&node, DID, session, &format!("{session}-r"), "completed").await;
        for (index, text) in texts.iter().enumerate() {
            seed_message(&node, DID, session, &request, index as u32 + 1, text).await;
        }
    }
    index_all_sessions(&node, &None, &CancellationToken::new())
        .await
        .unwrap();

    let tool = SessionHistoryTool::new(node.clone(), DID);
    let result = call(
        &tool,
        json!({"action":"search","mode":"semantic","query":"why did the schema migration roll back","limit":3}),
    )
    .await;
    assert_eq!(result["search_kind"], "semantic");
    let hits = result["hits"].as_array().unwrap();
    assert_eq!(hits[0]["session_id"], "infra");
    assert_eq!(hits[0]["sequence"], 1);
    assert!(hits[0]["excerpt"].as_str().unwrap().contains("rolled back"));
    assert!(hits[0]["message_doc_id"]
        .as_str()
        .unwrap()
        .starts_with("bae-"));
    assert!(hits[0]["score"].as_f64().unwrap() > hits[1]["score"].as_f64().unwrap());
    assert_eq!(result["index"]["sessions_with_pending_messages"], 0);

    let narrowed = call(
        &tool,
        json!({"action":"search","mode":"semantic","query":"images alt text","session_id":"frontend","limit":5}),
    )
    .await;
    assert!(narrowed["hits"]
        .as_array()
        .unwrap()
        .iter()
        .all(|hit| hit["session_id"] == "frontend"));
    assert_eq!(narrowed["hits"][0]["sequence"], 2);

    let literal = call(&tool, json!({"action":"search","query":"alt text"})).await;
    assert_eq!(literal["search_kind"], "literal_scan");
    assert_eq!(literal["hits"][0]["session_id"], "frontend");

    let help = call(&tool, json!({"action":"help","topic":"search"})).await;
    assert!(help["help"].as_str().unwrap().contains("semantic"));
    node.shutdown().await;
}

#[tokio::test]
async fn unreachable_embeddings_defer_indexing_and_name_the_literal_fallback() {
    let stub = EmbeddingStub::start().await;
    let node = node_with(&stub, None).await;
    seed_session(&node, DID, "s1", "Outage").await;
    let request = seed_request(&node, DID, "s1", "r1", "completed").await;
    seed_message(
        &node,
        DID,
        "s1",
        &request,
        1,
        "Transcript written during an outage.",
    )
    .await;
    stub.failing.store(true, Ordering::SeqCst);

    assert!(index_session(&node, &None, &key(DID, "s1")).await.is_err());
    assert!(chunk_sequences(&node, "s1").await.is_empty());
    assert_eq!(state(&node, "s1").await, None);
    let tool = SessionHistoryTool::new(node.clone(), DID);
    let error = Tool::call(
        &tool,
        serde_json::from_value(json!({"action":"search","mode":"semantic","query":"outage"}))
            .unwrap(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("bge-m3"), "{error}");
    let next: Value =
        serde_json::from_str(error.split_once("next call: sessions ").unwrap().1).unwrap();
    assert_eq!(next, json!({"action":"search","query":"outage"}));
    let literal = call(&tool, next).await;
    assert!(
        literal["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["sequence"] == 1),
        "{literal}"
    );

    stub.failing.store(false, Ordering::SeqCst);
    index_session(&node, &None, &key(DID, "s1")).await.unwrap();
    assert_eq!(chunk_sequences(&node, "s1").await, vec![1]);
    node.shutdown().await;
}

#[tokio::test]
async fn backfill_resumes_from_the_durable_watermark_after_restart() {
    let stub = EmbeddingStub::start().await;
    let data = tempfile::tempdir().unwrap();
    let node = node_with(&stub, Some(data.path())).await;
    seed_session(&node, DID, "long", "Long session").await;
    let request = seed_request(&node, DID, "long", "r1", "completed").await;
    for sequence in 1..=7 {
        seed_message(
            &node,
            DID,
            "long",
            &request,
            sequence,
            &format!("Step {sequence} of the long investigation."),
        )
        .await;
    }
    let first = index_session(&node, &None, &key(DID, "long"))
        .await
        .unwrap();
    assert!(first.more, "the test window is three messages");
    assert_eq!(state(&node, "long").await, Some((4, 0)));
    node.shutdown().await;
    drop(node);

    let node = node_with(&stub, Some(data.path())).await;
    let pass = index_all_sessions(&node, &None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(pass.indexed_messages, 4);
    assert_eq!(
        chunk_sequences(&node, "long").await,
        (1..=7).collect::<Vec<_>>()
    );
    assert_eq!(state(&node, "long").await, Some((8, 0)));
    node.shutdown().await;
}

#[tokio::test]
async fn crash_after_chunk_writes_replays_without_duplicates() {
    let stub = EmbeddingStub::start().await;
    let node = node_with(&stub, None).await;
    seed_session(&node, DID, "s1", "Crash").await;
    let request = seed_request(&node, DID, "s1", "r1", "completed").await;
    seed_message(&node, DID, "s1", &request, 1, "First answer.").await;
    seed_message(&node, DID, "s1", &request, 2, "Second answer.").await;
    index_session(&node, &None, &key(DID, "s1")).await.unwrap();
    ConfigAccess::write_local(
        &node,
        "test.retrieval.drop_state",
        &format!("mutation {{delete_{TRANSCRIPT_INDEX_STATE}(filter: {{}}) {{_docID}}}}"),
    )
    .await
    .unwrap();

    let replay = index_session(&node, &None, &key(DID, "s1")).await.unwrap();
    assert_eq!(replay.indexed_messages, 0);
    assert_eq!(chunk_sequences(&node, "s1").await, vec![1, 2]);
    assert_eq!(state(&node, "s1").await, Some((3, 0)));
    node.shutdown().await;
}

#[tokio::test]
async fn removed_sources_remove_their_projection() {
    let stub = EmbeddingStub::start().await;
    let node = node_with(&stub, None).await;
    seed_session(&node, DID, "gone", "Deleted later").await;
    let request = seed_request(&node, DID, "gone", "r1", "completed").await;
    seed_message(&node, DID, "gone", &request, 1, "Soon to be deleted.").await;
    seed_message(&node, DID, "gone", &request, 2, "Also deleted.").await;
    index_all_sessions(&node, &None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(chunk_sequences(&node, "gone").await, vec![1, 2]);

    ConfigAccess::write_local(
        &node,
        "test.retrieval.delete_session",
        r#"mutation {delete_AgentSession(filter: {session_id: {_eq: "gone"}}) {_docID}}"#,
    )
    .await
    .unwrap();
    let pass = index_all_sessions(&node, &None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(pass.removed_sessions, 1);
    assert!(chunk_sequences(&node, "gone").await.is_empty());
    assert_eq!(state(&node, "gone").await, None);
    node.shutdown().await;
}

#[tokio::test]
async fn empty_messages_count_as_indexed_without_embedding() {
    let stub = EmbeddingStub::start().await;
    let node = node_with(&stub, None).await;
    seed_session(&node, DID, "s1", "Empty").await;
    let request = seed_request(&node, DID, "s1", "r1", "completed").await;
    seed_message(&node, DID, "s1", &request, 1, "   ").await;
    seed_message(&node, DID, "s1", &request, 2, "Real text.").await;
    let pass = index_session(&node, &None, &key(DID, "s1")).await.unwrap();
    assert_eq!((pass.indexed_messages, pass.pending), (2, 0));
    assert_eq!(state(&node, "s1").await, Some((3, 0)));
    let tool = SessionHistoryTool::new(node.clone(), DID);
    let result = call(
        &tool,
        json!({"action":"search","mode":"semantic","query":"real text","limit":5}),
    )
    .await;
    assert_eq!(result["hits"].as_array().unwrap().len(), 1);
    node.shutdown().await;
}
