//! Exercises the production transaction adapter's commit/discard lifecycle.
//! This recorder does not establish atomic configuration snapshot publication;
//! that requires the runtime installer to implement ApplyReconcile.publish.

use super::*;
use axum::{extract::State, routing::post, Json, Router};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct RecordingGraphqlState {
    transactions: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
    committed: Arc<Mutex<Vec<String>>>,
    next_tx_id: Arc<AtomicU64>,
    tx_begin_count: Arc<AtomicU64>,
    tx_commit_count: Arc<AtomicU64>,
    tx_discard_count: Arc<AtomicU64>,
}

impl RecordingGraphqlState {
    fn committed_state(&self) -> Vec<String> {
        self.committed.lock().expect("committed lock").clone()
    }

    fn tx_lifecycle_counts(&self) -> (u64, u64, u64) {
        (
            self.tx_begin_count.load(Ordering::SeqCst),
            self.tx_commit_count.load(Ordering::SeqCst),
            self.tx_discard_count.load(Ordering::SeqCst),
        )
    }
}

async fn start_recording_graphql() -> (String, RecordingGraphqlState) {
    let state = RecordingGraphqlState::default();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind recording GraphQL listener");
    let addr = listener.local_addr().expect("recording GraphQL addr");
    let app = Router::new()
        .route("/api/v0/graphql", post(recording_graphql_handler))
        .route("/api/v0/tx", post(recording_tx_begin_handler))
        .route(
            "/api/v0/tx/{id}",
            post(recording_tx_commit_handler).delete(recording_tx_discard_handler),
        )
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("recording GraphQL server");
    });
    (format!("http://{addr}/api/v0/graphql"), state)
}

async fn recording_tx_begin_handler(State(state): State<RecordingGraphqlState>) -> Json<Value> {
    let id = state.next_tx_id.fetch_add(1, Ordering::SeqCst);
    state
        .transactions
        .lock()
        .expect("tx lock")
        .insert(id.to_string(), Vec::new());
    state.tx_begin_count.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "id": id.to_string() }))
}

async fn recording_tx_commit_handler(
    State(state): State<RecordingGraphqlState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::http::StatusCode {
    let mut transactions = state.transactions.lock().expect("tx lock");
    let Some(writes) = transactions.remove(&id) else {
        return axum::http::StatusCode::NOT_FOUND;
    };
    drop(transactions);
    state
        .committed
        .lock()
        .expect("committed lock")
        .extend(writes);
    state.tx_commit_count.fetch_add(1, Ordering::SeqCst);
    axum::http::StatusCode::OK
}

async fn recording_tx_discard_handler(
    State(state): State<RecordingGraphqlState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::http::StatusCode {
    let removed = state
        .transactions
        .lock()
        .expect("tx lock")
        .remove(&id)
        .is_some();
    if removed {
        state.tx_discard_count.fetch_add(1, Ordering::SeqCst);
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::NOT_FOUND
    }
}

async fn recording_graphql_handler(
    State(state): State<RecordingGraphqlState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let query = body["query"].as_str().expect("GraphQL query").to_string();
    let tx_id = headers
        .get("x-defradb-tx")
        .expect("production adapter must attach its transaction")
        .to_str()
        .expect("transaction header");
    state
        .transactions
        .lock()
        .expect("tx lock")
        .get_mut(tx_id)
        .expect("transaction must have begun")
        .push(query);
    Json(json!({ "data": { "doc_0": { "_docID": "task-doc" } } }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_txn_round_trip_against_recorder() {
    let (graphql, recorder) = start_recording_graphql().await;
    let access = ConfigAccess::Graphql(graphql);
    let recorder_in_tx = recorder.clone();
    access
        .transact("test.config_apply.round_trip", move |txn| {
            let recorder_in_tx = recorder_in_tx.clone();
            Box::pin(async move {
                txn.execute(
                    "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
                )
                .await?;
                assert!(recorder_in_tx.committed_state().is_empty());
                Ok(())
            })
        })
        .await
        .expect("owned transaction");

    let committed = recorder.committed_state();
    assert_eq!(committed.len(), 1);
    assert!(committed[0].contains("task-a"));
    let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
    assert_eq!((begin_count, commit_count, discard_count), (1, 1, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_txn_discard_leaves_committed_empty() {
    let (graphql, recorder) = start_recording_graphql().await;
    let access = ConfigAccess::Graphql(graphql);
    let result: Result<()> = access
        .transact("test.config_apply.discard", move |txn| {
            Box::pin(async move {
                txn.execute(
                    "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
                )
                .await?;
                anyhow::bail!("force rollback")
            })
        })
        .await;
    assert!(result.is_err());

    assert!(recorder.committed_state().is_empty());
    let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
    assert_eq!((begin_count, commit_count, discard_count), (1, 0, 1));
}
