use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use super::ConfigAccess;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, thiserror::Error)]
enum ReceiptProbeError {
    #[error("reading stable-id receipt")]
    Read(#[source] reqwest::Error),
    #[error("stable-id receipt status")]
    Status(#[source] reqwest::Error),
    #[error("decoding stable-id receipt")]
    Decode(#[source] reqwest::Error),
    #[error("stable-id receipt query")]
    GraphqlErrors(#[source] gents_protocol::database_error::GraphqlFailure),
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Post {
    Commits,
    LostReply,
    Conflict,
    Rejected,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Read {
    Answers,
    Transient,
    Fails,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Written,
    WriteFailed,
    ReceiptFailed,
}

#[derive(Clone)]
struct ScriptedWrite {
    posts: Arc<Vec<Post>>,
    reads: Arc<Vec<Read>>,
    mutation_count: Arc<AtomicUsize>,
    receipt_count: Arc<AtomicUsize>,
    durable: Arc<AtomicBool>,
}

fn rejection() -> Value {
    json!({"data": null, "errors": [{"message": "opaque"}]})
}

async fn scripted_graphql(
    State(state): State<ScriptedWrite>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let query = body
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if query.trim_start().starts_with("mutation") {
        let attempt = state.mutation_count.fetch_add(1, Ordering::SeqCst);
        match state.posts.get(attempt).copied().unwrap_or(Post::Commits) {
            Post::Commits => {
                state.durable.store(true, Ordering::SeqCst);
                Json(json!({"data": {"create_ReceiptFact": [{"_docID": "stable-id"}]}}))
            }
            Post::LostReply => {
                state.durable.store(true, Ordering::SeqCst);
                Json(transaction_conflict())
            }
            Post::Conflict => Json(transaction_conflict()),
            Post::Rejected => Json(rejection()),
        }
    } else {
        let attempt = state.receipt_count.fetch_add(1, Ordering::SeqCst);
        match state.reads.get(attempt).copied().unwrap_or(Read::Answers) {
            Read::Transient => Json(transaction_conflict()),
            Read::Fails => Json(rejection()),
            Read::Answers => {
                let rows = if state.durable.load(Ordering::SeqCst) {
                    vec![json!({"stable_id": "stable-id"})]
                } else {
                    Vec::new()
                };
                Json(json!({"data": {"ReceiptFact": rows}}))
            }
        }
    }
}

async fn start_server(state: ScriptedWrite) -> TestResult<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = Router::new()
        .route("/api/v0/graphql", post(scripted_graphql))
        .with_state(state);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((format!("http://{address}/api/v0/graphql"), server))
}

async fn receipt_once(endpoint: &str) -> Result<bool, ReceiptProbeError> {
    let response = reqwest::Client::new()
        .post(endpoint)
        .json(&json!({"query": "{ ReceiptFact { stable_id } }"}))
        .send()
        .await
        .map_err(ReceiptProbeError::Read)?
        .error_for_status()
        .map_err(ReceiptProbeError::Status)?
        .json::<Value>()
        .await
        .map_err(ReceiptProbeError::Decode)?;
    if let Some(errors) = response.get("errors") {
        return Err(ReceiptProbeError::GraphqlErrors(
            gents_protocol::database_error::GraphqlFailure::new("receipt", errors.clone()),
        ));
    }
    Ok(response
        .pointer("/data/ReceiptFact")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()))
}

const MUTATION: &str =
    r#"mutation { create_ReceiptFact(input: {stable_id: "stable-id"}) { _docID } }"#;

#[tokio::test]
async fn receipt_recovery_matches_the_lean_model() -> TestResult {
    use gents_lean_vocab::families::receipt_recovery_http as family;
    let cases = tokio::task::spawn_blocking(family::cases::<Outcome, Post, Read>).await?;
    let observed = futures::future::join_all(cases.iter().map(|case| async {
        let state = ScriptedWrite {
            posts: Arc::new(case.input.posts.clone()),
            reads: Arc::new(case.input.reads.clone()),
            mutation_count: Arc::new(AtomicUsize::new(0)),
            receipt_count: Arc::new(AtomicUsize::new(0)),
            durable: Arc::new(AtomicBool::new(false)),
        };
        let (endpoint, server) = start_server(state.clone()).await?;
        let result = ConfigAccess::graphql(endpoint.clone())
            .write_with_receipt("test.receipt", MUTATION, || receipt_once(&endpoint))
            .await;
        server.abort();
        TestResult::Ok(family::Observed {
            outcome: match result {
                Ok(()) => Outcome::Written,
                Err(super::ReceiptWriteError::Write(_)) => Outcome::WriteFailed,
                Err(super::ReceiptWriteError::Receipt(_)) => Outcome::ReceiptFailed,
            },
            posts: state.mutation_count.load(Ordering::SeqCst) as u64,
            reads: state.receipt_count.load(Ordering::SeqCst) as u64,
            durable: state.durable.load(Ordering::SeqCst),
        })
    }))
    .await;
    for (case, observed) in cases.iter().zip(observed) {
        case.check(observed?);
    }
    Ok(())
}

#[test]
fn delegated_bearer_remains_caller_owned_and_is_redacted() {
    let secret = "Bearer caller-signed-sensitive-token";
    let endpoint = super::GraphqlEndpoint::with_delegated_authorization(
        "http://127.0.0.1:8000/api/v0/graphql",
        secret,
    )
    .unwrap();
    assert_eq!(endpoint.authorization().unwrap().as_deref(), Some(secret));
    assert!(!format!("{endpoint:?}").contains(secret));
    assert_eq!(
        serde_json::to_value(&endpoint).unwrap(),
        serde_json::json!(endpoint.url())
    );
    for invalid in ["", "Bearer ", "Basic abc", "Bearer abc\nmalicious"] {
        assert!(
            super::GraphqlEndpoint::with_delegated_authorization(endpoint.url(), invalid).is_err()
        );
    }
}

/// The conflict reply as the pinned DefraDB serializes it.
fn transaction_conflict() -> Value {
    serde_json::to_value(query::QueryResponse::transaction_conflict(
        query::executor::TXN_CONFLICT_MESSAGE,
    ))
    .expect("serialize DefraDB conflict")
}
