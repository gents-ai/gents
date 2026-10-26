use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use gents::config_client::{ConfigAccess, GraphqlEndpoint, IdempotentTransactionRetry};
use serde_json::{json, Value};

#[derive(Default)]
struct Counts {
    begins: AtomicUsize,
    writes: AtomicUsize,
    commits: AtomicUsize,
    rollbacks: AtomicUsize,
}

#[derive(Clone)]
struct Failure {
    begin: bool,
    body: Value,
    counts: Arc<Counts>,
}

async fn begin(State(failure): State<Failure>) -> (StatusCode, Json<Value>) {
    failure.counts.begins.fetch_add(1, Ordering::SeqCst);
    if failure.begin {
        (StatusCode::INSUFFICIENT_STORAGE, Json(failure.body))
    } else {
        (StatusCode::OK, Json(json!({"id": "retained-transaction"})))
    }
}

async fn write(State(failure): State<Failure>) -> Json<Value> {
    failure.counts.writes.fetch_add(1, Ordering::SeqCst);
    Json(json!({"data": {"create_StorageProbe": [{"_docID": "probe"}]}}))
}

async fn commit(State(failure): State<Failure>) -> (StatusCode, Json<Value>) {
    failure.counts.commits.fetch_add(1, Ordering::SeqCst);
    (StatusCode::INSUFFICIENT_STORAGE, Json(failure.body))
}

async fn rollback(State(failure): State<Failure>) -> Json<Value> {
    failure.counts.rollbacks.fetch_add(1, Ordering::SeqCst);
    Json(json!({}))
}

#[tokio::test]
async fn storage_full_http_transactions_stop_without_replaying_or_discarding_consumed_handles() {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["storage_failure_cases"].as_array().unwrap();
    let full_cases: Vec<_> = cases.iter().filter(|case| case["full"] == true).collect();
    assert!(!full_cases.is_empty());
    for case in full_cases {
        assert!(case["stop_automatic_retry"].as_bool().unwrap());
        for fail_begin in [false, true] {
            let counts = Arc::new(Counts::default());
            let failure = Failure {
                begin: fail_begin,
                body: json!({"code": case["code"], "error": case["message"]}),
                counts: Arc::clone(&counts),
            };
            let app = Router::new()
                .route("/api/v0/tx", post(begin))
                .route("/api/v0/graphql", post(write))
                .route("/api/v0/tx/{id}", post(commit).delete(rollback))
                .with_state(failure);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
            let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            }));
            let access = ConfigAccess::Graphql(GraphqlEndpoint::anonymous(endpoint));
            let error = tokio::time::timeout(
                Duration::from_secs(10),
                access.transact_idempotent(
                    IdempotentTransactionRetry::FiveAttempts,
                    "test.storage_full.transaction",
                    |txn| {
                        Box::pin(async move {
                            txn.execute(
                                "mutation { create_StorageProbe(input: {value: 1}) { _docID } }",
                            )
                            .await?;
                            Ok::<_, gents::config_client::ConfigClientError>(())
                        })
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(
                gents::graphql::is_storage_full(&error),
                "{}",
                gents_protocol::error_chain::ErrorChain(&error)
            );
            assert_eq!(counts.begins.load(Ordering::SeqCst), 1);
            let entered = usize::from(!fail_begin);
            assert_eq!(counts.writes.load(Ordering::SeqCst), entered);
            assert_eq!(counts.commits.load(Ordering::SeqCst), entered);
            assert_eq!(
                counts.rollbacks.load(Ordering::SeqCst),
                0,
                "a returned commit failure consumed its handle"
            );
        }
    }
}
