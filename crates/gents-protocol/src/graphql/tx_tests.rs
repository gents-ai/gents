use super::transport::{execute_graphql_async_with_tx, graphql_http_client};
use super::*;
use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct HeaderRecorder {
    last_tx_header: Arc<Mutex<Option<String>>>,
}

#[derive(Clone)]
struct RetryRecorder {
    attempts: Arc<Mutex<usize>>,
    first_error_message: Arc<String>,
}

impl RetryRecorder {
    fn new(first_error_message: impl Into<String>) -> Self {
        Self {
            attempts: Arc::new(Mutex::new(0)),
            first_error_message: Arc::new(first_error_message.into()),
        }
    }
}

async fn capture_handler(
    State(state): State<HeaderRecorder>,
    headers: HeaderMap,
    Json(_body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    *state.last_tx_header.lock().unwrap() = headers
        .get("x-defradb-tx")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Json(serde_json::json!({ "data": {} }))
}

async fn retry_once_handler(
    State(state): State<RetryRecorder>,
    Json(_body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let mut attempts = state.attempts.lock().unwrap();
    *attempts += 1;
    if *attempts == 1 {
        return Json(serde_json::json!({
            "errors": [{
                "message": state.first_error_message.as_str()
            }]
        }));
    }
    Json(serde_json::json!({ "data": { "ok": true } }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_graphql_async_with_tx_sets_header_when_id_provided() {
    let state = HeaderRecorder::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(capture_handler))
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let endpoint = format!("http://{addr}/");
    let options = GraphqlRequestOptions {
        timeout: Some(std::time::Duration::from_secs(2)),
        max_attempts: 1,
        retry_backoff: std::time::Duration::from_millis(50),
    };

    execute_graphql_async_with_tx(
        &endpoint,
        None,
        "{ __typename }",
        None,
        options,
        Some("42"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(state.last_tx_header.lock().unwrap().as_deref(), Some("42"));

    execute_graphql_async_with_tx(&endpoint, None, "{ __typename }", None, options, None, None)
        .await
        .unwrap();
    assert_eq!(state.last_tx_header.lock().unwrap().as_deref(), None);
}

#[tokio::test]
async fn shared_graphql_client_preserves_per_request_timeouts() {
    let first = graphql_http_client().await.unwrap();
    let second = graphql_http_client().await.unwrap();
    assert!(std::ptr::eq(first, second));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/",
        post(|| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(serde_json::json!({ "data": { "ok": true } }))
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut options = GraphqlRequestOptions {
        timeout: Some(Duration::from_millis(10)),
        max_attempts: 1,
        retry_backoff: Duration::ZERO,
    };
    assert!(execute_graphql_async(&endpoint, None, "{ ok }", options)
        .await
        .is_err());
    options.timeout = None;
    assert_eq!(
        execute_graphql_async(&endpoint, None, "{ ok }", options)
            .await
            .unwrap()["data"]["ok"],
        true
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_transport_rejects_mutations_before_network_io() {
    let error = execute_graphql_async(
        "http://127.0.0.1:1/api/v0/graphql",
        None,
        "mutation { create_X(input: {}) { _docID } }",
        GraphqlRequestOptions::default(),
    )
    .await
    .expect_err("public transport is query-only")
    .to_string();
    assert!(
        error.contains("GraphQL read transport requires a query document"),
        "unexpected error: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_graphql_async_leaves_conflict_retry_to_the_server() {
    let state = RetryRecorder::new(
        "commit error: datastore error: storage error: transaction conflict. Please retry",
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(retry_once_handler))
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let endpoint = format!("http://{addr}/");
    let options = GraphqlRequestOptions {
        timeout: Some(std::time::Duration::from_secs(2)),
        max_attempts: 3,
        retry_backoff: std::time::Duration::from_millis(1),
    };

    let error = execute_graphql_async(&endpoint, None, "{ __typename }", options)
        .await
        .expect_err("a conflict the server returned is final");

    assert!(
        error.to_string().contains("transaction conflict"),
        "{error}"
    );
    assert_eq!(*state.attempts.lock().unwrap(), 1);
}

#[test]
fn retryable_graphql_conflicts_are_the_responses_defradb_reports() {
    let failure = |code: &str| {
        crate::database_error::GraphqlFailure::new(
            "test",
            serde_json::json!([{ "message": "opaque", "extensions": { "code": code } }]),
        )
    };
    #[derive(Debug, thiserror::Error)]
    #[error("outer shim context")]
    struct Outer(#[source] crate::database_error::GraphqlFailure);
    #[derive(Debug, thiserror::Error)]
    #[error("outer protocol context")]
    struct OuterProtocol(#[source] GraphqlError);

    let conflict = || failure(crate::database_error::TXN_CONFLICT_CODE);
    assert!(graphql_error_is_retryable(&conflict()));
    assert!(graphql_error_is_retryable(&Outer(conflict())));
    assert!(graphql_error_is_retryable(&GraphqlError::Database(
        conflict()
    )));
    assert!(graphql_error_is_retryable(&OuterProtocol(
        GraphqlError::Database(conflict())
    )));
    assert!(!graphql_transport_error_in_chain(&GraphqlError::Database(
        conflict()
    )));
    assert!(!graphql_error_is_retryable(&GraphqlError::Database(
        failure("TXN_RETRY_EXHAUSTED")
    )));

    let worded = crate::database_error::GraphqlFailure::new(
        "test",
        serde_json::json!([{ "message": "transaction conflict. Please retry" }]),
    );
    assert!(
        !graphql_error_is_retryable(&GraphqlError::Database(worded)),
        "conflict wording without DefraDB's code is not a retryable conflict"
    );
}

#[tokio::test]
async fn http_authentication_refusals_preserve_endpoint_availability() {
    for (status, available) in [
        (axum::http::StatusCode::OK, true),
        (axum::http::StatusCode::UNAUTHORIZED, true),
        (axum::http::StatusCode::FORBIDDEN, true),
        (axum::http::StatusCode::NOT_FOUND, false),
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let router = axum::Router::new().route(
            "/api/v0/graphql",
            axum::routing::post(move || async move { status }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let options = super::GraphqlRequestOptions {
            max_attempts: 1,
            ..Default::default()
        };
        assert_eq!(
            super::graphql_endpoint_available(&endpoint, options).await,
            available,
            "{status}"
        );
        if matches!(
            status,
            axum::http::StatusCode::UNAUTHORIZED | axum::http::StatusCode::FORBIDDEN
        ) {
            assert!(
                super::execute_graphql_async(&endpoint, None, "{ __typename }", options)
                    .await
                    .is_err()
            );
        }
        server.abort();
        server.await.unwrap_err();
    }
}

#[tokio::test]
async fn a_refused_credential_keeps_the_delegation_audience_edge_code() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/",
            post(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "code": crate::delegation_audience::DELEGATION_REQUIRED_CODE,
                        "error": "refused",
                    })),
                )
            }),
        )
        .route(
            "/plain",
            post(|| async { (axum::http::StatusCode::FORBIDDEN, "no") }),
        )
        .route(
            "/long",
            post(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    format!(
                        r#"{{"code":"delegationRequired","pad":"{}"}}"#,
                        "x".repeat(8192)
                    ),
                )
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let options = GraphqlRequestOptions {
        timeout: None,
        max_attempts: 1,
        retry_backoff: Duration::from_millis(10),
    };
    let refused = execute_graphql_async(&endpoint, Some("Bearer t"), "{ __typename }", options)
        .await
        .unwrap_err();
    assert_eq!(
        refused.refusal_code(),
        Some((
            reqwest::StatusCode::UNAUTHORIZED,
            crate::delegation_audience::DELEGATION_REQUIRED_CODE
        ))
    );
    assert_eq!(
        refused.transport_error().and_then(reqwest::Error::status),
        Some(reqwest::StatusCode::UNAUTHORIZED),
        "the refusal keeps its status for every classifier that reads it"
    );
    for path in ["plain", "long"] {
        let refused = execute_graphql_async(
            &format!("{endpoint}{path}"),
            Some("Bearer t"),
            "{ __typename }",
            options,
        )
        .await
        .unwrap_err();
        assert_eq!(refused.refusal_code(), None, "{path}");
        assert!(refused.transport_error().is_some(), "{path}");
    }
}
