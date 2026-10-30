use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn routing_affinity_matches_lean_owner() {
    use crate::oauth_http::OAuthHttpPolicy;
    use gents_loop::provider_input::routing_affinity::{scope, RoutingAffinity};
    for case in &crate::lean_vocab_test::lean_contract_snapshot().routing_affinity_cases {
        let state = RoutingAffinity::start(
            case["codex"].as_bool().unwrap(),
            case["main"].as_bool().unwrap(),
            case["session"].as_str().map(str::to_owned),
        );
        scope(state.clone(), async {
            for response in case["responses"].as_array().unwrap() {
                let mut headers = HeaderMap::new();
                if let Some(token) = response.as_str() {
                    headers.insert("x-codex-turn-state", HeaderValue::from_str(token).unwrap());
                }
                retain_turn_state(&headers);
            }
            let req = Request::builder()
                .uri("https://chatgpt.com/backend-api/codex/responses")
                .header("session_id", "stale-session")
                .header("x-client-request-id", "stale-session")
                .header("x-codex-turn-state", "stale-token")
                .body(Bytes::from_static(b"{}"))
                .unwrap();
            let req = ChatGptCodexPolicy::default().patch_request_body(req);
            assert_eq!(
                req.headers()
                    .get("session_id")
                    .and_then(|h| h.to_str().ok()),
                case["expected_session"].as_str(),
                "{case}"
            );
            assert_eq!(
                req.headers()
                    .get("x-client-request-id")
                    .and_then(|h| h.to_str().ok()),
                case["expected_session"].as_str(),
                "{case}"
            );
            assert_eq!(
                req.headers()
                    .get("x-codex-turn-state")
                    .and_then(|h| h.to_str().ok()),
                case["expected_token"].as_str(),
                "{case}"
            );
        })
        .await;
    }
}

#[tokio::test]
async fn routing_affinity_isolates_concurrent_executions_without_capture() {
    use gents_loop::provider_input::routing_affinity::{current, scope, RoutingAffinity};
    let first = RoutingAffinity::start(true, true, Some("same-session".into())).unwrap();
    let second = RoutingAffinity::start(true, true, Some("same-session".into())).unwrap();
    let run = |state: Arc<RoutingAffinity>, token: &'static str| async move {
        scope(Some(state.clone()), async move {
            assert!(crate::rendered_request::scope::current_scope().is_none());
            let mut headers = HeaderMap::new();
            headers.insert("x-codex-turn-state", HeaderValue::from_static(token));
            retain_turn_state(&headers);
            tokio::task::yield_now().await;
            scope(None, async {
                assert!(current().is_none());
                retain_turn_state(&headers);
            })
            .await;
            assert_eq!(current().unwrap().token(), Some(token));
            use crate::oauth_http::OAuthHttpPolicy;
            let other = Request::builder()
                .uri("https://api.x.ai/v1/responses")
                .header("x-custom", "unchanged")
                .body(Bytes::from_static(b"{}"))
                .unwrap();
            let original_headers = other.headers().clone();
            let other = crate::xai_grok_oauth::XaiGrokOAuthPolicy.patch_request_body(other);
            assert_eq!(other.headers(), &original_headers);

            let mut later = HeaderMap::new();
            later.insert("x-codex-turn-state", HeaderValue::from_static("later"));
            retain_turn_state(&later);
            assert_eq!(state.token(), Some(token));
        })
        .await;
    };
    tokio::join!(run(first, "first"), run(second, "second"));
    assert!(current().is_none());
    let next = RoutingAffinity::start(true, true, Some("same-session".into())).unwrap();
    assert_eq!(next.token(), None);
}

struct CountingBearer {
    token: String,
    calls: AtomicUsize,
    invalidations: AtomicUsize,
}

impl CountingBearer {
    fn new(token: &str) -> Arc<Self> {
        Arc::new(Self {
            token: token.to_string(),
            calls: AtomicUsize::new(0),
            invalidations: AtomicUsize::new(0),
        })
    }
}

impl BearerSource for CountingBearer {
    async fn current_bearer(&self) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.token.clone())
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
struct StatusInjectingClient {
    status: u16,
}

impl HttpClientExt for StatusInjectingClient {
    fn send<T, U>(
        &self,
        _req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let status = self.status;
        async move {
            if status >= 400 {
                return Err(http_client::Error::InvalidStatusCodeWithMessage(
                    status.to_string().parse().expect("valid status"),
                    "injected".to_string(),
                ));
            }
            let body: LazyBody<U> = Box::pin(async { Ok(U::from(Bytes::new())) });
            Response::builder()
                .header("x-codex-turn-state", "buffered-first")
                .status(status)
                .body(body)
                .map_err(http_client::Error::Protocol)
        }
    }

    fn send_multipart<U>(
        &self,
        _req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        std::future::ready(Err(http_client::Error::InvalidStatusCode(
            "501".parse().expect("valid status"),
        )))
    }

    fn send_streaming<T>(
        &self,
        _req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let status = self.status;
        async move {
            if status >= 400 {
                return Err(http_client::Error::InvalidStatusCode(
                    status.to_string().parse().unwrap(),
                ));
            }
            let body: rig::http_client::sse::BoxedStream = Box::pin(futures::stream::empty());
            Response::builder()
                .status(status)
                .header("x-codex-turn-state", "streaming-first")
                .body(body)
                .map_err(http_client::Error::Protocol)
        }
    }
}

#[tokio::test]
async fn codex_affinity_transport_captures_first_response_and_reuses_it_on_followup() {
    use crate::oauth_http::OAuthHttpPolicy;
    use gents_loop::provider_input::routing_affinity::{scope, RoutingAffinity};
    let client = ChatGptCodexHttpClient::with_inner(
        CountingBearer::new("test-bearer"),
        StatusInjectingClient { status: 200 },
    );
    let state = RoutingAffinity::start(true, true, Some("session".into())).unwrap();
    scope(Some(state.clone()), async {
        let request = || {
            Request::builder()
                .uri("https://chatgpt.com/backend-api/codex/responses")
                .body(Bytes::from_static(b"{}"))
                .unwrap()
        };
        let initial = client.prepare_for_test(request()).await.unwrap();
        assert_eq!(initial.headers()["session_id"], "session");
        assert!(!initial.headers().contains_key("x-codex-turn-state"));
        HttpClientExt::send::<Bytes, Bytes>(&client, request())
            .await
            .unwrap();
        assert_eq!(state.token(), Some("buffered-first"));
        let body: rig::http_client::sse::BoxedStream = Box::pin(futures::stream::empty());
        let response = Response::builder()
            .header("x-codex-turn-state", "streaming-later")
            .body(body)
            .unwrap();
        ChatGptCodexPolicy::default().patch_streaming(response);
        let followup = client.prepare_for_test(request()).await.unwrap();
        assert_eq!(followup.headers()["x-codex-turn-state"], "buffered-first");
        assert!(followup.headers()["x-codex-turn-state"].is_sensitive());
        assert_eq!(followup.headers()["x-client-request-id"], "session");
    })
    .await;
    let fresh = RoutingAffinity::start(true, true, Some("session".into())).unwrap();
    scope(Some(fresh.clone()), async {
        let request = Request::builder()
            .uri("https://chatgpt.com/backend-api/codex/responses")
            .body(Bytes::from_static(b"{}"))
            .unwrap();
        assert_eq!(fresh.token(), None);
        HttpClientExt::send_streaming(&client, request.clone())
            .await
            .unwrap();
        assert_eq!(fresh.token(), Some("streaming-first"));
        let resend = client.prepare_for_test(request.clone()).await.unwrap();
        assert_eq!(resend.headers()["x-codex-turn-state"], "streaming-first");
        HttpClientExt::send_streaming(&client, request)
            .await
            .unwrap();
        assert_eq!(fresh.token(), Some("streaming-first"));
    })
    .await;
    let unscoped = client
        .prepare_for_test(
            Request::builder()
                .uri("https://chatgpt.com/backend-api/codex/responses")
                .body(Bytes::from_static(b"{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!unscoped.headers().contains_key("session_id"));
    assert!(!unscoped.headers().contains_key("x-codex-turn-state"));
}

#[tokio::test]
async fn the_captured_row_is_the_body_codex_rewrote_not_the_one_rig_serialized() {
    use crate::rendered_request::scope::{arm, scope_request, test_scope, CaptureScopeKind};
    use crate::rendered_request::transport::CountingInner;
    use crate::rendered_request::{
        AssemblyBuildPath, AssemblyTrace, RenderedCompletionRequest, RenderedRequestCaptureSink,
        RenderedRequestContext,
    };
    use std::sync::Mutex;

    let terminal = CountingInner::default();
    let client = ChatGptCodexHttpClient::with_inner(
        CountingBearer::new("tok"),
        crate::rendered_request::RenderedRequestCapturingHttpClient::new(terminal.clone()),
    );

    let seen: Arc<Mutex<Vec<RenderedCompletionRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let sink: RenderedRequestCaptureSink = Arc::new(move |rendered| {
        let seen = Arc::clone(&sink_seen);
        Box::pin(async move {
            seen.lock().expect("seen").push(rendered);
            Ok(())
        })
    });
    let scope = test_scope(
        RenderedRequestContext {
            request_doc_id: "doc-1".to_string(),
            request_commit_cid: "bafy-request-commit".to_string(),
            request_id: "req-1".to_string(),
            node_did: "did:key:agent".to_string(),
            requester_did: String::new(),
            agent_id: "behavior".to_string(),
            session_id: "session".to_string(),
            model_name: "configured-model".to_string(),
            provider_family: None,
        },
        sink,
    );

    let assembled = serde_json::json!({
        "model": "gpt-5-codex",
        "prompt_cache_key": "session",
        "input": [
            {"role": "system", "content": [{"type": "input_text", "text": "you are gents"}]},
            {"role": "user", "content": [{"type": "input_text", "text": "hi"}]},
        ],
        "tools": [{"type": "function", "name": "read", "strict": true}],
        "temperature": 0.7,
        "max_output_tokens": 4096,
    });

    scope_request(scope, async {
        arm(
            CaptureScopeKind::Inference,
            0,
            0,
            AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new()),
        )
        .expect("armed");
        let req = Request::builder()
            .method("POST")
            .uri("https://chatgpt.com/backend-api/codex/responses")
            .body(Bytes::from(
                serde_json::to_vec(&assembled).expect("assembled body"),
            ))
            .expect("request");
        let _ = HttpClientExt::send_streaming(&client, req)
            .await
            .expect("send");
    })
    .await;

    let seen = seen.lock().expect("seen");
    assert_eq!(seen.len(), 1, "exactly one row for one completion body");
    let captured = &seen[0].request_json;
    assert_eq!(captured["prompt_cache_key"], "session");

    assert_eq!(
        captured["instructions"], "you are gents",
        "the row must show the hoisted instructions, not the assembled `input` system item"
    );
    assert!(
        captured["input"]
            .as_array()
            .expect("input array")
            .iter()
            .all(|item| item["role"] != "system"),
        "system items are stripped from `input` before the send: {captured}"
    );
    assert_eq!(captured["store"], false);
    assert_eq!(captured["stream"], true);
    assert_eq!(captured["tools"][0]["strict"], false);
    assert!(
        captured.get("temperature").is_none() && captured.get("max_output_tokens").is_none(),
        "params Codex deletes must be absent from the row: {captured}"
    );

    assert_eq!(
        terminal.bodies().first().expect("a forwarded body"),
        captured,
        "the persisted row and the bytes the network client received must be identical"
    );
    assert_eq!(terminal.send_count(), 1);
}

async fn send_through(status: u16) -> Arc<CountingBearer> {
    let bearer = CountingBearer::new("tok");
    let client =
        ChatGptCodexHttpClient::with_inner(bearer.clone(), StatusInjectingClient { status });
    let req = Request::builder()
        .method("POST")
        .uri("https://example.com/v1/models")
        .body(Bytes::from_static(b"{}"))
        .unwrap();
    let _ = HttpClientExt::send::<Bytes, Bytes>(&client, req).await;
    bearer
}

#[tokio::test]
async fn provider_401_invalidates_the_bearer() {
    let bearer = send_through(401).await;
    assert_eq!(
        bearer.invalidations.load(Ordering::SeqCst),
        1,
        "a 401 from the provider must invalidate the bearer so the next request refreshes"
    );
}

#[tokio::test]
async fn provider_success_leaves_the_bearer_intact() {
    let bearer = send_through(200).await;
    assert_eq!(
        bearer.invalidations.load(Ordering::SeqCst),
        0,
        "a successful response must not invalidate the bearer"
    );
}

#[tokio::test]
async fn injects_fresh_bearer_on_each_request() {
    let bearer = CountingBearer::new("tok-123");
    let client = ChatGptCodexHttpClient::new(bearer.clone());

    let req = Request::builder()
        .method("POST")
        .uri("https://example.com/v1/responses")
        .header("authorization", "Bearer STALE")
        .body(Bytes::from_static(b"{}"))
        .unwrap();

    let prepared = client.prepare_for_test(req).await.unwrap();
    let auth = prepared
        .headers()
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();

    assert_eq!(auth, "Bearer tok-123", "stale bearer was replaced");
    assert_eq!(
        bearer.calls.load(Ordering::SeqCst),
        1,
        "refreshed once per request"
    );
}

#[test]
fn chatgpt_codex_headers_advertise_codex_version_and_sse_accept() {
    let headers = build_chatgpt_codex_headers(Some("acct_123"), true).unwrap();

    assert_eq!(
        headers.get("version").and_then(|value| value.to_str().ok()),
        Some(chatgpt_codex_client_version().as_str())
    );
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("text/event-stream, application/json")
    );
    assert_eq!(
        headers
            .get("chatgpt-account-id")
            .and_then(|value| value.to_str().ok()),
        Some("acct_123")
    );
    assert_eq!(
        headers
            .get("x-openai-fedramp")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(
        headers
            .get("originator")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs")
    );
}

#[test]
fn default_client_version_supports_gpt_6_1_sol_catalog() {
    let parse = |version: &str| -> Vec<u32> {
        version
            .split('.')
            .map(|part| part.parse().unwrap())
            .collect()
    };
    assert_eq!(CHATGPT_CODEX_CLIENT_VERSION, "0.160.0");
    // Codex rust-v0.160.0, a956835d020762cb2b570053af06f643a11c0ecc:
    // models-manager/models.json gates gpt-6.1-sol at 0.153.0.
    assert!(parse(CHATGPT_CODEX_CLIENT_VERSION) >= parse("0.153.0"));
}

#[test]
fn login_tokens_project_claims_without_codex_auth_types() {
    use base64::Engine as _;

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "exp": 1_900_000_000i64,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-local",
                "chatgpt_account_is_fedramp": true,
                "chatgpt_plan_type": "pro"
            }
        }))
        .expect("payload"),
    );
    let id_token = format!("e30.{payload}.signature");
    let credential = OAuthCredential::from_login_tokens(
        "did:key:zAgent",
        CHATGPT_CODEX_PROVIDER,
        &id_token,
        "access".to_string(),
        "refresh".to_string(),
        Utc::now(),
    );
    assert_eq!(credential.account_id.as_deref(), Some("acct-local"));
    assert_eq!(credential.chatgpt_plan_type.as_deref(), Some("pro"));
    assert!(credential.is_fedramp);
    assert_eq!(credential.id_token.as_deref(), Some(id_token.as_str()));
}

fn chatgpt_login(id_claims: Value, access_token: String) -> OAuthCredential {
    OAuthCredential::from_login_tokens(
        "did:key:zAgent",
        CHATGPT_CODEX_PROVIDER,
        &crate::oauth_credential::test_support::unsigned_jwt(id_claims),
        access_token,
        "refresh".to_string(),
        Utc::now(),
    )
}

fn access_token(auth: Value) -> String {
    crate::oauth_credential::test_support::unsigned_jwt(
        json!({ "https://api.openai.com/auth": auth }),
    )
}

#[test]
fn login_tokens_store_the_member_account_key() {
    let id_claims = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" } });
    for member in ["member-a", "member-b"] {
        let credential = chatgpt_login(
            id_claims.clone(),
            access_token(json!({
                "chatgpt_account_id": "acct-ws",
                "chatgpt_account_user_id": member
            })),
        );
        assert_eq!(credential.account_id.as_deref(), Some("acct-ws"));
        assert_eq!(credential.provider_account_key.as_deref(), Some(member));
    }
}

#[test]
fn login_tokens_fall_back_to_workspace_and_user() {
    let no_member = || access_token(json!({ "chatgpt_account_id": "acct-ws" }));
    let with_user_id = json!({ "https://api.openai.com/auth": {
        "chatgpt_account_id": "acct-ws",
        "chatgpt_user_id": "user-a"
    } });
    let with_sub = json!({
        "sub": "user-a",
        "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" }
    });
    let other_workspace = access_token(json!({
        "chatgpt_account_id": "acct-other",
        "chatgpt_account_user_id": "member-b"
    }));
    for (id_claims, access) in [
        (with_user_id, no_member()),
        (with_sub.clone(), no_member()),
        (with_sub, other_workspace),
    ] {
        let credential = chatgpt_login(id_claims, access);
        assert_eq!(
            credential.provider_account_key.as_deref(),
            Some("acct-ws:user-a")
        );
    }
}

#[test]
fn login_tokens_without_readable_claims_have_no_key() {
    let no_user = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" } });
    let no_workspace = json!({ "sub": "user-a" });
    let member = access_token(json!({
        "chatgpt_account_id": "acct-ws",
        "chatgpt_account_user_id": "member-a"
    }));
    for (id_claims, access) in [(no_user, "not-a-jwt".to_string()), (no_workspace, member)] {
        assert_eq!(chatgpt_login(id_claims, access).provider_account_key, None);
    }
}

#[test]
fn streamed_output_prefers_deltas_over_done_text() {
    let sse = concat!(
        "event: response.output_text.delta
",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"pong\"}
",
        "
",
        "event: response.output_text.done
",
        "data: {\"type\":\"response.output_text.done\",\"text\":\"pong\"}
",
        "
"
    );

    assert_eq!(streamed_output_text(sse), "pong");
}
