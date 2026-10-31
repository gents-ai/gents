//! Wire-level tests of the Claude Messages transport against a loopback
//! plain-text server: the exact request head, gzip with padding and its
//! rejection latch, the 401 replay, and usage observation.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use gents_claude::request::RequestHead;
use gents_claude::retry::{DEFAULT_MAX_RETRIES, OAUTH_REFRESH_ATTEMPTS};
use rapidhash::{HashSetExt, RapidHashSet};
use rig::completion::CompletionError;
use rig::http_client::{HttpClientExt, MultipartForm, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::lock_fixtures_for_test;
use super::send::{stream_messages_at, Replay};
use super::tests::{echo_request, test_account};
use super::transport::{capturing, ClaudeHttpClient};
use crate::oauth_credential::BearerSource;
use gents_loop::claude_messages_body::ClaudeAccountContext;

const SSE_DONE: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

pub(super) struct Reply {
    pub(super) status: &'static str,
    pub(super) headers: &'static [(&'static str, &'static str)],
    pub(super) body: &'static str,
}

pub(super) const fn reply(status: &'static str, body: &'static str) -> Reply {
    Reply {
        status,
        headers: &[],
        body,
    }
}

const UNAUTHORIZED: Reply = reply(
    "401 Unauthorized",
    r#"{"type":"error","error":{"type":"authentication_error","message":"bad token"}}"#,
);

/// Serves `replies` in order, one connection each, and records every raw request.
pub(super) struct Server {
    pub(super) port: u16,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Server {
    pub(super) async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let raw = read_request(&mut socket).await;
                record.lock().expect("seen").push(raw);
                let extra: String = reply
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect();
                let text = format!(
                    "HTTP/1.1 {}\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body
                );
                socket.write_all(text.as_bytes()).await.expect("write");
                socket.shutdown().await.ok();
            }
        });
        Self { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/messages?beta=true", self.port)
    }

    pub(super) fn requests(&self) -> Vec<Wire> {
        self.seen
            .lock()
            .expect("seen")
            .iter()
            .map(|raw| Wire::parse(raw))
            .collect()
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = socket.read(&mut chunk).await.expect("read");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + length {
                return buf;
            }
        }
        if n == 0 {
            return buf;
        }
    }
}

/// One request as it crossed the wire.
pub(super) struct Wire {
    head: Vec<u8>,
    pub(super) request_line: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl Wire {
    fn parse(raw: &[u8]) -> Self {
        let end = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("head") + 4;
        let text = std::str::from_utf8(&raw[..end]).expect("ascii head");
        let mut lines = text.trim_end().split("\r\n");
        let request_line = lines.next().expect("request line").to_owned();
        let headers = lines
            .map(|line| {
                let (name, value) = line.split_once(": ").expect("header");
                (name.to_owned(), value.to_owned())
            })
            .collect();
        Self {
            head: raw[..end].to_vec(),
            request_line,
            headers,
            body: raw[end..].to_vec(),
        }
    }

    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Returns scripted tokens in order, then repeats the last; `Err` entries fail the refresh.
struct ScriptedBearer {
    script: Mutex<VecDeque<Result<String, String>>>,
    last: Mutex<Result<String, String>>,
    invalidations: AtomicUsize,
}

impl ScriptedBearer {
    fn new(script: &[Result<&str, &str>]) -> Self {
        let owned: VecDeque<Result<String, String>> = script
            .iter()
            .map(|entry| entry.map(str::to_owned).map_err(str::to_owned))
            .collect();
        Self {
            last: Mutex::new(owned.front().cloned().expect("script")),
            script: Mutex::new(owned),
            invalidations: AtomicUsize::new(0),
        }
    }
}

impl BearerSource for ScriptedBearer {
    async fn current_bearer(&self) -> anyhow::Result<String> {
        let mut script = self.script.lock().expect("script");
        let next = script
            .pop_front()
            .unwrap_or_else(|| self.last.lock().expect("last").clone());
        *self.last.lock().expect("last") = next.clone();
        next.map_err(|message| anyhow::anyhow!(message))
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

async fn open(
    url: &str,
    account: &ClaudeAccountContext,
    bearer: &ScriptedBearer,
) -> Result<(), CompletionError> {
    stream_messages_at(
        url,
        "claude-sonnet-5",
        &echo_request(),
        account,
        RapidHashSet::new(),
        bearer,
        None,
    )
    .await
    .map(|_| ())
}

#[tokio::test]
async fn a_main_turn_goes_out_with_bun_s_exact_head() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![reply("200 OK", SSE_DONE)]).await;
    let bearer = ScriptedBearer::new(&[Ok("access-A")]);
    open(&server.url(), &test_account(), &bearer)
        .await
        .expect("200");

    let wire = &server.requests()[0];
    assert_eq!(wire.request_line, "POST /v1/messages?beta=true HTTP/1.1");
    assert_eq!(wire.header("authorization"), Some("Bearer access-A"));
    assert_eq!(
        wire.header("host"),
        Some(format!("127.0.0.1:{}", server.port).as_str())
    );
    assert_eq!(
        wire.header("content-length"),
        Some(wire.body.len().to_string().as_str())
    );
    assert!(wire.header("content-encoding").is_none());

    // Everything before Bun's appended defaults is sorted by code point.
    let names: Vec<&str> = wire.headers.iter().map(|(name, _)| name.as_str()).collect();
    let user_end = names
        .iter()
        .position(|n| *n == "Connection")
        .expect("connection");
    assert!(names[..user_end]
        .windows(2)
        .all(|w| w[0].as_bytes() <= w[1].as_bytes()));
    assert_eq!(
        &names[user_end..],
        ["Connection", "Host", "Accept-Encoding", "Content-Length"]
    );
    assert!(names.contains(&"Authorization") && names.contains(&"Content-Type"));

    // The whole head is what RequestHead renders for the headers the caller supplied.
    let supplied: Vec<(String, String)> = wire.headers[..user_end]
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect();
    let rendered = RequestHead::new(
        "POST",
        "/v1/messages?beta=true",
        &format!("127.0.0.1:{}", server.port),
        supplied
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str(), false)),
        false,
    )
    .encode(wire.body.len());
    assert_eq!(
        String::from_utf8_lossy(&wire.head),
        String::from_utf8_lossy(&rendered)
    );
}

fn big_body(units: usize) -> String {
    format!(r#"{{"pad":"{}"}}"#, "x".repeat(units))
}

fn first_party_request(port: u16, body: &str) -> Request<Bytes> {
    Request::builder()
        .method("POST")
        .uri(format!(
            "http://api.anthropic.com:{port}/v1/messages?beta=true"
        ))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_owned()))
        .expect("request")
}

fn routed(latch: &Arc<AtomicBool>, port: u16) -> ClaudeHttpClient {
    ClaudeHttpClient::new(None).with_test_route(latch.clone(), "127.0.0.1", port)
}

/// Runs `future` under an armed capture scope whose sink counts the bodies it was given.
async fn with_capture<F>(future: F) -> usize
where
    F: std::future::Future<Output = ()>,
{
    use crate::rendered_request::scope::{arm, scope_request, test_scope};
    use crate::rendered_request::{
        AssemblyBuildPath, AssemblyTrace, CaptureScopeKind, RenderedRequestCaptureSink,
        RenderedRequestContext,
    };
    let captured = Arc::new(AtomicUsize::new(0));
    let count = captured.clone();
    let sink: RenderedRequestCaptureSink = Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    });
    let scope = test_scope(
        RenderedRequestContext {
            request_doc_id: "doc".into(),
            request_commit_cid: "cid".into(),
            request_id: "request".into(),
            node_did: "did:test:node".into(),
            requester_did: String::new(),
            agent_id: "general".into(),
            session_id: "session".into(),
            model_name: "claude".into(),
            provider_family: None,
        },
        sink,
    );
    scope_request(scope, async {
        arm(
            CaptureScopeKind::Inference,
            0,
            0,
            AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new()),
        )
        .expect("arm capture");
        future.await;
    })
    .await;
    captured.load(Ordering::SeqCst)
}

#[tokio::test]
async fn a_large_first_party_body_goes_gzipped_with_padding_after_capture() {
    let body = big_body(4100);
    let server = Server::start(vec![reply("200 OK", SSE_DONE)]).await;
    let latch = Arc::new(AtomicBool::new(false));
    let client = capturing(None, routed(&latch, server.port));
    let captured = with_capture(async {
        client
            .send_streaming(first_party_request(server.port, &body))
            .await
            .expect("200");
    })
    .await;
    // Capture parses the body as JSON, which a gzip stream would fail.
    assert_eq!(captured, 1);

    let wire = &server.requests()[0];
    assert_eq!(wire.header("content-encoding"), Some("gzip"));
    assert_eq!(
        wire.header("content-length"),
        Some(wire.body.len().to_string().as_str())
    );
    let mut plain = String::new();
    flate2::read::GzDecoder::new(&wire.body[..])
        .read_to_string(&mut plain)
        .expect("gzip");
    let padding = plain.strip_prefix(&body).expect("body first");
    let padding = padding.strip_prefix('\n').expect("newline");
    assert!(padding.len() <= 257, "{}", padding.len());
    assert!(padding.chars().all(|c| c == ' ' || c == '\t'));
}

#[tokio::test]
async fn a_small_first_party_body_goes_plain() {
    let body = big_body(100);
    let server = Server::start(vec![reply("200 OK", SSE_DONE)]).await;
    let latch = Arc::new(AtomicBool::new(false));
    routed(&latch, server.port)
        .send_streaming(first_party_request(server.port, &body))
        .await
        .expect("200");
    let wire = &server.requests()[0];
    assert!(wire.header("content-encoding").is_none());
    assert_eq!(wire.body, body.as_bytes());
}

#[tokio::test]
async fn a_gzip_rejection_latches_gzip_off_and_resends_plain_once() {
    let body = big_body(4100);
    let server = Server::start(vec![
        Reply {
            status: "400 Bad Request",
            headers: &[("request-id", "req_011")],
            body: r#"{"type":"error","error":{"type":"invalid_request_error","message":"The request body is not valid JSON"}}"#,
        },
        reply("200 OK", SSE_DONE),
        reply("200 OK", SSE_DONE),
    ])
    .await;
    let latch = Arc::new(AtomicBool::new(false));
    let client = routed(&latch, server.port);
    client
        .send_streaming(first_party_request(server.port, &body))
        .await
        .expect("the plain resend succeeds");
    assert!(latch.load(Ordering::SeqCst));

    let seen = server.requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].header("content-encoding"), Some("gzip"));
    assert!(seen[1].header("content-encoding").is_none());
    assert_eq!(seen[1].body, body.as_bytes());

    client
        .send_streaming(first_party_request(server.port, &body))
        .await
        .expect("200");
    let third = &server.requests()[2];
    assert!(third.header("content-encoding").is_none(), "latched off");
}

#[tokio::test]
async fn a_refreshed_token_replays_the_same_body_with_new_credentials() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![UNAUTHORIZED, reply("200 OK", SSE_DONE)]).await;
    let bearer = ScriptedBearer::new(&[Ok("access-OLD"), Ok("access-NEW")]);
    let captured = with_capture(async {
        open(&server.url(), &test_account(), &bearer)
            .await
            .expect("the replay succeeds");
    })
    .await;
    assert_eq!(captured, 1, "the resend reuses the durable capture");

    let seen = server.requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].header("authorization"), Some("Bearer access-OLD"));
    assert_eq!(seen[1].header("authorization"), Some("Bearer access-NEW"));
    assert_ne!(
        seen[0].header("x-client-request-id"),
        seen[1].header("x-client-request-id")
    );
    assert!(seen[1].header("x-client-request-id").is_some());
    assert_eq!(
        seen[0].header("x-claude-code-session-id"),
        seen[1].header("x-claude-code-session-id")
    );
    assert_eq!(seen[0].body, seen[1].body);
    assert_eq!(bearer.invalidations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_unchanged_token_fails_after_the_refresh_attempts() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![UNAUTHORIZED, UNAUTHORIZED, reply("200 OK", SSE_DONE)]).await;
    let bearer = ScriptedBearer::new(&[Ok("access-STALE")]);
    let error = open(&server.url(), &test_account(), &bearer)
        .await
        .expect_err("401");
    assert!(error.to_string().contains("401"), "{error}");
    assert_eq!(server.requests().len(), OAUTH_REFRESH_ATTEMPTS as usize);
    assert_eq!(
        bearer.invalidations.load(Ordering::SeqCst),
        OAUTH_REFRESH_ATTEMPTS as usize
    );
}

#[tokio::test]
async fn an_api_key_401_is_not_replayed() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![UNAUTHORIZED, reply("200 OK", SSE_DONE)]).await;
    let bearer = ScriptedBearer::new(&[Ok("key-1"), Ok("key-2")]);
    let account = ClaudeAccountContext {
        subscriber: false,
        api_key: true,
        ..test_account()
    };
    let error = open(&server.url(), &account, &bearer)
        .await
        .expect_err("401");
    assert!(error.to_string().contains("401"), "{error}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn a_refresh_failure_ends_the_request_with_its_error() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![UNAUTHORIZED, reply("200 OK", SSE_DONE)]).await;
    let bearer = ScriptedBearer::new(&[Ok("access-OLD"), Err("the Claude account is on hold")]);
    let error = open(&server.url(), &test_account(), &bearer)
        .await
        .expect_err("hold");
    assert!(error.to_string().contains("on hold"), "{error}");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn replay_is_bounded_by_the_retry_budget_and_the_unchanged_token_count() {
    let mut replay = Replay::default();
    for sent in 1..=DEFAULT_MAX_RETRIES + 1 {
        replay.sent();
        assert_eq!(replay.may_resend(), sent <= DEFAULT_MAX_RETRIES, "{sent}");
    }

    let mut replay = Replay::default();
    replay.sent();
    assert!(replay.refreshed(false, 0.0).is_some());
    assert!(
        replay.refreshed(true, 0.0).is_some(),
        "a new token resets the count"
    );
    assert!(replay.refreshed(false, 0.0).is_some());
    assert!(replay.refreshed(false, 0.0).is_none());
}

#[tokio::test]
async fn usage_headers_are_observed_on_success_and_on_error() {
    use crate::usage_observation::{UsageAccount, UsageCache, UsageReporter};
    use gents_loop::account_usage::UsageSource;
    let node = crate::oauth_credential::test_support::test_node().await;
    let cache: Arc<UsageCache> = Arc::default();
    for (index, (status, source)) in [
        ("200 OK", UsageSource::Header),
        ("429 Too Many Requests", UsageSource::Error),
    ]
    .into_iter()
    .enumerate()
    {
        let account = UsageAccount::Backend {
            node_did: "did:key:z6MkWireUsage".into(),
            provider: "ClaudeCliSubscription".into(),
            backend_id: format!("backend-wire-{index}"),
            connection: format!("connection-wire-{index}"),
        };
        let server = Server::start(vec![Reply {
            status,
            headers: &[
                ("anthropic-ratelimit-unified-5h-utilization", "0.5"),
                ("anthropic-ratelimit-unified-5h-reset", "1790354400"),
            ],
            body: "{}",
        }])
        .await;
        let reporter = UsageReporter::new(cache.clone(), node.clone(), account.clone());
        let latch = Arc::new(AtomicBool::new(false));
        let client =
            ClaudeHttpClient::new(Some(reporter)).with_test_route(latch, "127.0.0.1", server.port);
        let _ = client
            .send_streaming(first_party_request(server.port, "{}"))
            .await;
        let stored = crate::provider_http::tests::stored_usage_eventually(&cache, &node, &account)
            .await
            .expect("usage recorded");
        let window = &stored.report.windows[0];
        assert_eq!((window.label.as_str(), window.used_pct), ("5h", 50.0));
        assert_eq!(window.source, source);
    }
}

#[tokio::test]
async fn multipart_is_a_typed_error() {
    let client = ClaudeHttpClient::new(None);
    let request = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1:1/v1/messages")
        .body(MultipartForm::new())
        .expect("request");
    let error = client
        .send_multipart::<Bytes>(request)
        .await
        .err()
        .expect("multipart");
    assert!(error.to_string().contains("multipart"), "{error}");
}
