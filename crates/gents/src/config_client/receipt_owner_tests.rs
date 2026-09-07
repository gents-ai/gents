use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use super::ConfigAccess;

#[derive(Clone, Copy)]
enum Scenario {
    CommitThenAmbiguous,
    AbsentThenCommit,
    ReceiptTransientThenPresent,
}

#[derive(Clone)]
struct ScriptedWrite {
    scenario: Scenario,
    mutation_count: Arc<AtomicUsize>,
    receipt_count: Arc<AtomicUsize>,
    durable: Arc<AtomicBool>,
}

impl ScriptedWrite {
    fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            mutation_count: Arc::new(AtomicUsize::new(0)),
            receipt_count: Arc::new(AtomicUsize::new(0)),
            durable: Arc::new(AtomicBool::new(false)),
        }
    }
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
        let attempt = state.mutation_count.fetch_add(1, Ordering::SeqCst) + 1;
        match state.scenario {
            Scenario::CommitThenAmbiguous | Scenario::ReceiptTransientThenPresent => {
                state.durable.store(true, Ordering::SeqCst);
                Json(json!({"errors": [{"message": "database is locked"}]}))
            }
            Scenario::AbsentThenCommit if attempt == 1 => {
                Json(json!({"errors": [{"message": "database is locked"}]}))
            }
            Scenario::AbsentThenCommit => {
                state.durable.store(true, Ordering::SeqCst);
                Json(json!({"data": {"create_ReceiptFact": [{"_docID": "stable-id"}]}}))
            }
        }
    } else {
        let attempt = state.receipt_count.fetch_add(1, Ordering::SeqCst) + 1;
        if matches!(state.scenario, Scenario::ReceiptTransientThenPresent) && attempt == 1 {
            return Json(json!({"errors": [{"message": "database is locked"}]}));
        }
        let rows = if state.durable.load(Ordering::SeqCst) {
            vec![json!({"stable_id": "stable-id"})]
        } else {
            Vec::new()
        };
        Json(json!({"data": {"ReceiptFact": rows}}))
    }
}

async fn start_server(state: ScriptedWrite) -> Result<(String, tokio::task::JoinHandle<()>)> {
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

async fn receipt_once(endpoint: &str) -> Result<bool> {
    let response = reqwest::Client::new()
        .post(endpoint)
        .json(&json!({"query": "{ ReceiptFact { stable_id } }"}))
        .send()
        .await
        .context("reading stable-id receipt")?
        .error_for_status()
        .context("stable-id receipt status")?
        .json::<Value>()
        .await
        .context("decoding stable-id receipt")?;
    if let Some(errors) = response.get("errors") {
        anyhow::bail!("graphql returned errors: {errors}");
    }
    Ok(response
        .pointer("/data/ReceiptFact")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()))
}

const MUTATION: &str =
    r#"mutation { create_ReceiptFact(input: {stable_id: "stable-id"}) { _docID } }"#;

#[tokio::test]
async fn confirmed_receipt_recovers_without_reposting() -> Result<()> {
    let state = ScriptedWrite::new(Scenario::CommitThenAmbiguous);
    let (endpoint, server) = start_server(state.clone()).await?;
    let access = ConfigAccess::Graphql(endpoint.clone());

    access
        .write_with_receipt("test.receipt_recovered", MUTATION, || {
            receipt_once(&endpoint)
        })
        .await?;

    assert_eq!(state.mutation_count.load(Ordering::SeqCst), 1);
    assert_eq!(state.receipt_count.load(Ordering::SeqCst), 1);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn successful_absent_receipt_allows_one_repost() -> Result<()> {
    let state = ScriptedWrite::new(Scenario::AbsentThenCommit);
    let (endpoint, server) = start_server(state.clone()).await?;
    let access = ConfigAccess::Graphql(endpoint.clone());

    access
        .write_with_receipt("test.receipt_absent", MUTATION, || receipt_once(&endpoint))
        .await?;

    assert_eq!(state.mutation_count.load(Ordering::SeqCst), 2);
    assert_eq!(state.receipt_count.load(Ordering::SeqCst), 1);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn transient_receipt_read_retries_without_reposting() -> Result<()> {
    let state = ScriptedWrite::new(Scenario::ReceiptTransientThenPresent);
    let (endpoint, server) = start_server(state.clone()).await?;
    let access = ConfigAccess::Graphql(endpoint.clone());

    access
        .write_with_receipt("test.receipt_read_retry", MUTATION, || {
            receipt_once(&endpoint)
        })
        .await?;

    assert_eq!(state.mutation_count.load(Ordering::SeqCst), 1);
    assert_eq!(state.receipt_count.load(Ordering::SeqCst), 2);
    server.abort();
    Ok(())
}
