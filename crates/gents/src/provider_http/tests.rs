use std::time::Duration;

use futures::StreamExt;
use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::error::{classify_completion_error, InferenceError};

/// Answers one request with `status`, the extra `headers`, and `body`.
pub(crate) async fn one_shot_server(
    status: &'static str,
    headers: &'static [(&'static str, &'static str)],
    body: &'static str,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = socket.read(&mut chunk).await.expect("read");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_ascii_lowercase();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= end + 4 + length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let extra: String = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.expect("write");
        socket.shutdown().await.ok();
    });
    url
}

fn classify_http(error: http_client::Error) -> InferenceError {
    classify_completion_error(&rig::agent::StreamingError::Completion(
        rig::completion::CompletionError::HttpError(error),
    ))
}

fn post(url: &str) -> Request<Bytes> {
    Request::builder()
        .method("POST")
        .uri(format!("{url}/v1/responses"))
        .body(Bytes::from_static(b"{}"))
        .expect("request")
}

#[tokio::test]
async fn streaming_429_keeps_anthropic_unified_reset() {
    let url = one_shot_server(
        "429 Too Many Requests",
        &[
            ("retry-after", "6000"),
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1790354400"),
        ],
        r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."}}"#,
    )
    .await;
    let error = ProviderHttpClient::default()
        .send_streaming(post(&url))
        .await
        .err()
        .expect("429");
    let text = error.to_string();
    assert!(text.contains("Invalid status code 429"), "{text}");
    assert!(
        text.ends_with("reset-at=2026-09-25T16:40:00Z usage-exhausted]"),
        "{text}"
    );
    match classify_http(error) {
        InferenceError::UsageLimited(limit) => {
            assert_eq!(
                limit.resets_at.map(|at| at.timestamp()),
                Some(1_790_354_400)
            );
        }
        other => panic!("expected usage limit, got {other:?}"),
    }
}

#[tokio::test]
async fn buffered_429_honors_short_retry_after() {
    let url = one_shot_server(
        "429 Too Many Requests",
        &[("retry-after", "3")],
        r#"{"code":"Too many requests","error":"slow down"}"#,
    )
    .await;
    let error = ProviderHttpClient::default()
        .send::<Bytes, Bytes>(post(&url))
        .await
        .err()
        .expect("429");
    match classify_http(error) {
        InferenceError::RateLimited {
            retry_after: Some(wait),
        } => assert!(
            wait <= Duration::from_secs(3) && wait >= Duration::from_secs(2),
            "{wait:?}"
        ),
        other => panic!("expected throttle, got {other:?}"),
    }
}

#[tokio::test]
async fn success_passes_the_body_through() {
    let url = one_shot_server("200 OK", &[], r#"{"ok":true}"#).await;
    let response = ProviderHttpClient::default()
        .send::<Bytes, Bytes>(post(&url))
        .await
        .expect("200");
    assert_eq!(
        response.into_body().await.expect("body"),
        Bytes::from_static(br#"{"ok":true}"#)
    );
}

/// Rig's Responses stream reduces a transport error to
/// `ProviderError(error.to_string())`; the Codex usage-limit body and its reset
/// still reach the classifier.
#[tokio::test]
async fn codex_usage_limit_survives_rig_responses_stream() {
    let url = one_shot_server(
        "429 Too Many Requests",
        &[
            ("x-codex-primary-used-percent", "100"),
            ("x-codex-primary-reset-at", "1790350000"),
        ],
        r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1790354400}}"#,
    )
    .await;
    let client = crate::inference_http::build_openai_responses_client(
        "test-key",
        &format!("{url}/v1"),
        ProviderHttpClient::default(),
        Default::default(),
    )
    .expect("client");
    let model = client.completion_model("gpt-5");
    let request = model.completion_request("hi").build();
    let error = match model.stream(request).await {
        Err(error) => error,
        Ok(mut stream) => stream
            .next()
            .await
            .expect("stream item")
            .err()
            .expect("usage-limit error"),
    };
    match classify_completion_error(&rig::agent::StreamingError::Completion(error)) {
        InferenceError::UsageLimited(limit) => {
            assert_eq!(
                limit.resets_at.map(|at| at.timestamp()),
                Some(1_790_354_400)
            );
            assert_eq!(limit.detail, "The usage limit has been reached");
        }
        other => panic!("expected usage limit, got {other:?}"),
    }
}

/// Seats without extra usage report overage `rejected` on ordinary responses;
/// an overloaded 529 stays a transient transport failure.
#[tokio::test]
async fn overloaded_529_with_overage_rejected_stays_transient() {
    let url = one_shot_server(
        "529 Overloaded",
        &[
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("anthropic-ratelimit-unified-overage-status", "rejected"),
            ("anthropic-ratelimit-unified-overage-reset", "1790354400"),
        ],
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    )
    .await;
    let error = ProviderHttpClient::default()
        .send_streaming(post(&url))
        .await
        .err()
        .expect("529");
    match classify_http(error) {
        InferenceError::TransientFailure { reason } => {
            assert!(!reason.contains("provider-limit"), "{reason}");
        }
        other => panic!("expected transient failure, got {other:?}"),
    }
}

const USAGE_DID: &str = "did:key:z6MkUsageA";

fn usage_account(backend_id: &str) -> crate::usage_observation::UsageAccount {
    crate::usage_observation::UsageAccount::Backend {
        agent_did: USAGE_DID.to_string(),
        provider: "ChatGptCodex".to_string(),
        backend_id: backend_id.to_string(),
    }
}

async fn usage_client(
    backend_id: &str,
) -> (std::sync::Arc<defra_node::EmbeddedNode>, ProviderHttpClient) {
    let node = std::sync::Arc::new(crate::oauth_credential::test_support::test_node().await);
    let reporter =
        crate::usage_observation::UsageReporter::new(node.clone(), usage_account(backend_id));
    (
        node,
        ProviderHttpClient::with_usage(ReqwestClient::new(), reporter),
    )
}

/// The usage write is spawned: poll the store for up to five seconds.
pub(crate) async fn stored_usage_eventually(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    account: &crate::usage_observation::UsageAccount,
) -> Option<crate::usage_observation::StoredUsage> {
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let stored = crate::usage_observation::load_usage(&access, account)
            .await
            .expect("load usage");
        if stored.is_some() || tokio::time::Instant::now() >= deadline {
            return stored;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const CODEX_USAGE_HEADERS: &[(&str, &str)] = &[
    ("x-codex-primary-used-percent", "12"),
    ("x-codex-primary-window-minutes", "300"),
];

#[tokio::test]
async fn usage_capture_success_send_records_header_windows() {
    let (node, client) = usage_client("backend-usage-send").await;
    let url = one_shot_server("200 OK", CODEX_USAGE_HEADERS, r#"{"ok":true}"#).await;
    client.send::<Bytes, Bytes>(post(&url)).await.expect("200");

    let stored = stored_usage_eventually(&node, &usage_account("backend-usage-send"))
        .await
        .expect("usage recorded");
    let window = &stored.report.windows[0];
    assert_eq!((window.label.as_str(), window.used_pct), ("primary", 12.0));
    assert_eq!(
        window.source,
        gents_loop::account_usage::UsageSource::Header
    );
}

#[tokio::test]
async fn usage_capture_streaming_success_records_header_windows() {
    let (node, client) = usage_client("backend-usage-stream").await;
    let url = one_shot_server("200 OK", CODEX_USAGE_HEADERS, "data: {}\n\n").await;
    let response = client.send_streaming(post(&url)).await.expect("200");
    let mut body = response.into_body();
    while body.next().await.is_some() {}

    let stored = stored_usage_eventually(&node, &usage_account("backend-usage-stream"))
        .await
        .expect("usage recorded");
    assert_eq!(stored.report.windows[0].used_pct, 12.0);
}

#[tokio::test]
async fn usage_capture_rejected_records_error_source_and_keeps_the_marker() {
    let (node, client) = usage_client("backend-usage-rejected").await;
    let url = one_shot_server(
        "429 Too Many Requests",
        &[
            ("x-codex-primary-used-percent", "100"),
            ("x-codex-primary-reset-at", "1790354400"),
        ],
        r#"{"error":{"type":"usage_limit_reached"}}"#,
    )
    .await;
    let error = client.send_streaming(post(&url)).await.err().expect("429");
    assert!(
        error
            .to_string()
            .ends_with("reset-at=2026-09-25T16:40:00Z usage-exhausted]"),
        "{error}"
    );

    let stored = stored_usage_eventually(&node, &usage_account("backend-usage-rejected"))
        .await
        .expect("usage recorded");
    let window = &stored.report.windows[0];
    assert_eq!(window.used_pct, 100.0);
    assert_eq!(window.source, gents_loop::account_usage::UsageSource::Error);
}

#[tokio::test]
async fn usage_capture_without_reporter_writes_nothing() {
    let node = std::sync::Arc::new(crate::oauth_credential::test_support::test_node().await);
    let url = one_shot_server("200 OK", CODEX_USAGE_HEADERS, r#"{"ok":true}"#).await;
    ProviderHttpClient::default()
        .send::<Bytes, Bytes>(post(&url))
        .await
        .expect("200");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let rows = crate::config_client::ConfigAccess::Local(node)
        .execute("{ ProviderAccountUsage { usage_key } }")
        .await
        .expect("read");
    assert_eq!(rows["data"]["ProviderAccountUsage"], serde_json::json!([]));
}

#[tokio::test]
async fn usage_capture_write_failure_never_fails_the_call() {
    let node = std::sync::Arc::new(
        defra_node::EmbeddedNode::builder()
            .build()
            .await
            .expect("node without the usage schema"),
    );
    let reporter =
        crate::usage_observation::UsageReporter::new(node, usage_account("backend-usage-failure"));
    let client = ProviderHttpClient::with_usage(ReqwestClient::new(), reporter);
    let url = one_shot_server("200 OK", CODEX_USAGE_HEADERS, r#"{"ok":true}"#).await;
    let response = client
        .send::<Bytes, Bytes>(post(&url))
        .await
        .expect("the call succeeds");
    assert_eq!(
        response.into_body().await.expect("body"),
        Bytes::from_static(br#"{"ok":true}"#)
    );
}
