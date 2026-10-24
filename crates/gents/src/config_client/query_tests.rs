use gents_protocol::error_chain::ErrorChain;
use std::time::Duration;

use axum::{http::StatusCode, routing::post, Router};
use tokio::net::TcpListener;

use super::{query_graphql_with_options, GraphqlEndpoint, GraphqlRequestOptions};

fn one_attempt(timeout: Duration) -> GraphqlRequestOptions {
    GraphqlRequestOptions {
        timeout,
        max_attempts: 1,
        retry_backoff: Duration::ZERO,
    }
}

fn transport_source(error: &anyhow::Error) -> Option<&reqwest::Error> {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<reqwest::Error>())
}

fn assert_diagnostic(error: &anyhow::Error, endpoint: &GraphqlEndpoint) {
    let message = error.to_string();
    assert!(message.contains(endpoint.url()), "{message}");
    assert_eq!(message.matches("\nNext:\n").count(), 1, "{message}");
    assert!(message.contains("gents server"), "{message}");
    assert!(!message.starts_with("Next:"), "the cause precedes the hint");
}

#[tokio::test]
async fn graphql_connection_errors_keep_their_source_and_diagnostic() {
    let endpoint = GraphqlEndpoint::anonymous("http://127.0.0.1:0/graphql");
    let error = query_graphql_with_options(
        &endpoint,
        "{ __typename }",
        one_attempt(Duration::from_secs(5)),
    )
    .await
    .expect_err("destination port zero cannot host a listener");
    let source = transport_source(&error).expect("connection error remains downcastable");
    assert!(source.is_connect(), "{}", ErrorChain(error.as_ref()));
    assert!(!source.is_timeout(), "{}", ErrorChain(error.as_ref()));
    assert_eq!(source.url().map(reqwest::Url::as_str), Some(endpoint.url()));
    assert_diagnostic(&error, &endpoint);
}

#[tokio::test]
async fn graphql_timeouts_keep_their_source_and_diagnostic() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        GraphqlEndpoint::anonymous(format!("http://{}/graphql", listener.local_addr().unwrap()));
    let error = query_graphql_with_options(
        &endpoint,
        "{ __typename }",
        one_attempt(Duration::from_millis(100)),
    )
    .await
    .expect_err("the HTTP response never arrives");
    let source = transport_source(&error).expect("timeout remains downcastable");
    assert!(source.is_timeout(), "{}", ErrorChain(error.as_ref()));
    assert_eq!(source.url().map(reqwest::Url::as_str), Some(endpoint.url()));
    assert_diagnostic(&error, &endpoint);
}

#[tokio::test]
async fn graphql_response_errors_remain_distinct_from_transport_failures() {
    for (kind, status, body) in [
        ("status", StatusCode::FORBIDDEN, "denied"),
        ("decode", StatusCode::OK, "not JSON"),
        (
            "graphql",
            StatusCode::OK,
            r#"{"errors":[{"message":"the field named timeout is invalid"}]}"#,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = GraphqlEndpoint::anonymous(format!(
            "http://{}/graphql",
            listener.local_addr().unwrap()
        ));
        let app = Router::new().route("/graphql", post(move || async move { (status, body) }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = query_graphql_with_options(
            &endpoint,
            "{ __typename }",
            one_attempt(Duration::from_secs(5)),
        )
        .await;
        server.abort();
        let _ = server.await;
        let error = response.expect_err(kind);
        assert_diagnostic(&error, &endpoint);
        match (kind, transport_source(&error)) {
            ("status", Some(source)) => {
                assert_eq!(source.status(), Some(StatusCode::FORBIDDEN));
                assert!(!source.is_connect() && !source.is_timeout() && !source.is_body());
            }
            ("decode", Some(source)) => {
                assert!(source.is_decode());
                assert!(!source.is_connect() && !source.is_timeout() && !source.is_body());
            }
            ("graphql", None) => assert!(error.to_string().contains("timeout")),
            _ => panic!(
                "{kind} changed classification: {}",
                ErrorChain(error.as_ref())
            ),
        }
    }
}
