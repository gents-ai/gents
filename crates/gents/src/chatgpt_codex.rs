use std::future::Future;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, Request, ReqwestClient, Response,
};
#[cfg(test)]
use rig::http_client::{MultipartForm, StreamingResponse};
use rig::wasm_compat::WasmCompatSend;
use serde_json::{json, Value};

#[cfg(test)]
use crate::oauth_credential::{classify_chatgpt_auth_error, BearerSource, OAuthAuthProblem};
use crate::oauth_credential::{
    oauth_credential_id, DbCredentialBearer, OAuthCredential, OAuthRefreshKind,
    CHATGPT_OAUTH_PRODUCT,
};

pub const CHATGPT_CODEX_PROVIDER: &str = "chatgpt-codex";
const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

pub fn default_backend_endpoint() -> &'static str {
    CHATGPT_CODEX_BASE_URL
}

impl OAuthCredential {
    pub fn from_login_tokens(
        agent_did: impl Into<String>,
        provider: impl Into<String>,
        id_token: &str,
        access_token: String,
        refresh_token: String,
        now: DateTime<Utc>,
    ) -> Self {
        let agent_did = agent_did.into();
        let provider = provider.into();
        let id_claims = crate::chatgpt_oauth_refresh::decode_id_token_claims(id_token);
        let access_token_expires_at = crate::chatgpt_oauth_refresh::jwt_expiration(&access_token)
            .or(id_claims.expires_at)
            .unwrap_or_else(|| now + Duration::hours(1));
        Self {
            doc_id: None,
            credential_id: oauth_credential_id(&agent_did, &provider),
            agent_did,
            provider,
            access_token,
            refresh_token,
            id_token: Some(id_token.to_string()),
            account_id: id_claims.account_id,
            chatgpt_plan_type: id_claims.plan_type,
            is_fedramp: id_claims.is_fedramp,
            access_token_expires_at,
            last_refresh: Some(now),
            enabled: true,
        }
    }
}

pub fn normalize_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default_backend_endpoint().to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn normalize_provider(provider: &str) -> String {
    let provider = provider.trim();
    if provider.is_empty() {
        CHATGPT_CODEX_PROVIDER.to_string()
    } else {
        provider.to_string()
    }
}

pub fn build_chatgpt_codex_headers(
    account_id: Option<&str>,
    is_fedramp: bool,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
    headers.insert(
        "User-Agent",
        HeaderValue::from_static(concat!("gents/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert(
        "Accept",
        HeaderValue::from_static("text/event-stream, application/json"),
    );
    if let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) {
        let account_id = HeaderValue::from_str(account_id)
            .context("ChatGPT account id could not be encoded as an HTTP header")?;
        headers.insert("ChatGPT-Account-ID", account_id);
    }
    if is_fedramp {
        headers.insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
    }
    headers.insert(
        "version",
        HeaderValue::from_str(&chatgpt_codex_client_version())
            .context("ChatGPT Codex client version could not be encoded as an HTTP header")?,
    );
    Ok(headers)
}

pub fn chatgpt_codex_client_version() -> String {
    std::env::var(CHATGPT_CODEX_CLIENT_VERSION_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CHATGPT_CODEX_CLIENT_VERSION.to_string())
}

// The codex backend-api gates its /models list by the advertised client
// version, so an old default silently hides newer model families (#982).
// Keep this at a current codex CLI release when bumping the vendored codex
// crate rev; GENTS_CHATGPT_CODEX_CLIENT_VERSION overrides it at runtime.
const CHATGPT_CODEX_CLIENT_VERSION: &str = "0.144.4";
const CHATGPT_CODEX_CLIENT_VERSION_ENV: &str = "GENTS_CHATGPT_CODEX_CLIENT_VERSION";

/// [`crate::oauth_http::IdentityHeaders`] policy for the ChatGPT Codex
/// Responses transport: 401/403 kill the bearer, the `/responses` body needs
/// the `instructions` hoist, and a buffered send must rewrite Codex's SSE
/// body into the JSON `response.completed` shape rig expects. Codex's
/// identity headers (`build_chatgpt_codex_headers`) ride as `http_headers`
/// client-build defaults instead of a per-request merge, so this policy
/// leaves `merge_identity_headers` at its no-op default.
#[derive(Clone, Default)]
pub struct ChatGptCodexPolicy;

impl crate::oauth_http::IdentityHeaders for ChatGptCodexPolicy {
    const REJECTION_STATUSES: &'static [u16] = &[401, 403];

    fn patch_request_body(&self, req: Request<Bytes>) -> Request<Bytes> {
        let (parts, body) = req.into_parts();
        let mut body = body;
        if parts.uri.path().ends_with("/responses") {
            if let Some(patched) = patch_instructions_body(&body) {
                body = patched;
            }
        }
        Request::from_parts(parts, body)
    }

    fn send_via<T, U>(
        &self,
        inner: &T,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Clone + HttpClientExt + 'static,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = inner.clone();
        async move {
            let is_responses_request = req.uri().path().ends_with("/responses");
            let request_body = req.body().clone();
            let response = HttpClientExt::send::<Bytes, Bytes>(&inner, req).await?;

            let status = response.status();
            let headers = response.headers().clone();
            let response_body = response.into_body().await?;
            let body = if is_responses_request {
                let text = String::from_utf8_lossy(&response_body);
                synthesize_completion_response(&request_body, &text)
            } else {
                response_body
            };

            let mut response_builder = Response::builder().status(status);
            if let Some(response_headers) = response_builder.headers_mut() {
                *response_headers = headers;
            }
            let body: LazyBody<U> = Box::pin(async move { Ok(U::from(body)) });
            response_builder
                .body(body)
                .map_err(http_client::Error::Protocol)
        }
    }
}

/// Codex's Responses transport: bearer auth, the `instructions` hoist, and
/// the SSE→JSON response rewrite, layered over `crate::oauth_http`'s shared
/// bearer-auth wrapper.
pub type ChatGptCodexHttpClient<S, H = ReqwestClient> =
    crate::oauth_http::BearerAuthHttpClient<S, ChatGptCodexPolicy, H>;

pub(crate) fn patch_instructions_body(body: &[u8]) -> Option<Bytes> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    let mut changed = false;

    if value.get("instructions").is_none() {
        let instructions = first_system_text(value.get("input")?)?;
        value["instructions"] = Value::String(instructions);
        if let Some(input) = value.get_mut("input") {
            strip_system_items(input);
        }
        changed = true;
    }
    if value.get("store").is_none() {
        value["store"] = Value::Bool(false);
        changed = true;
    }
    if value.get("stream").is_none() {
        value["stream"] = Value::Bool(true);
        changed = true;
    }
    for unsupported in CHATGPT_CODEX_UNSUPPORTED_PARAMS {
        if let Some(object) = value.as_object_mut() {
            if object.remove(*unsupported).is_some() {
                changed = true;
            }
        }
    }
    if let Some(tools) = value.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(object) = tool.as_object_mut() {
                if object.get("strict") != Some(&Value::Bool(false)) {
                    object.insert("strict".to_string(), Value::Bool(false));
                    changed = true;
                }
            }
        }
    }
    if !changed {
        return None;
    }
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

const CHATGPT_CODEX_UNSUPPORTED_PARAMS: &[&str] = &["max_output_tokens", "temperature", "top_p"];

fn first_system_text(input: &Value) -> Option<String> {
    match input {
        Value::Array(items) => items.iter().find_map(system_item_text),
        Value::Object(_) => system_item_text(input),
        _ => None,
    }
}

fn system_item_text(item: &Value) -> Option<String> {
    if item.get("role").and_then(Value::as_str) != Some("system") {
        return None;
    }
    content_text(item.get("content")?)
}

fn strip_system_items(input: &mut Value) {
    match input {
        Value::Array(items) => {
            items.retain(|item| item.get("role").and_then(Value::as_str) != Some("system"));
        }
        Value::Object(item) if item.get("role").and_then(Value::as_str) == Some("system") => {
            item.clear();
        }
        Value::Object(_) => {}
        _ => {}
    }
}

fn synthesize_completion_response(request_body: &[u8], sse_body: &str) -> Bytes {
    if let Some(response) = completed_response(sse_body) {
        if let Ok(body) = serde_json::to_vec(&response) {
            return Bytes::from(body);
        }
    }

    let model = serde_json::from_slice::<Value>(request_body)
        .ok()
        .and_then(|request| {
            request
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "gpt-5.2".to_string());
    let text = streamed_output_text(sse_body);
    let response = json!({
        "id": "gents-chatgpt-codex-response",
        "object": "response",
        "created_at": chrono::Utc::now().timestamp().max(0) as u64,
        "status": "completed",
        "error": null,
        "incomplete_details": null,
        "instructions": null,
        "max_output_tokens": null,
        "model": model,
        "usage": null,
        "output": [
            {
                "type": "message",
                "id": "gents-chatgpt-codex-message",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": text
                    }
                ]
            }
        ]
    });
    Bytes::from(serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec()))
}

fn completed_response(sse_body: &str) -> Option<Value> {
    sse_events(sse_body).into_iter().find_map(|event| {
        if event.get("type").and_then(Value::as_str) == Some("response.completed") {
            event
                .get("response")
                .filter(|response| response.get("output").is_some())
                .cloned()
        } else {
            None
        }
    })
}

fn streamed_output_text(sse_body: &str) -> String {
    let mut deltas = String::new();
    let mut done_text = None;
    for event in sse_events(sse_body) {
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    deltas.push_str(delta);
                }
            }
            Some("response.output_text.done") => {
                done_text = event
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            _ => {}
        }
    }
    if deltas.is_empty() {
        done_text.unwrap_or_default()
    } else {
        deltas
    }
}

fn sse_events(sse_body: &str) -> Vec<Value> {
    sse_body
        .split("\n\n")
        .filter_map(|event| {
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                return None;
            }
            serde_json::from_str::<Value>(&data).ok()
        })
        .collect()
}

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        Value::Object(part) => part
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

pub async fn build_responses_client(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    endpoint: &str,
) -> Result<
    rig::providers::openai::Client<
        ChatGptCodexHttpClient<
            DbCredentialBearer,
            crate::rendered_request::RenderedRequestCapturingHttpClient,
        >,
    >,
> {
    let provider = CHATGPT_CODEX_PROVIDER;
    let (bearer, credential) = crate::oauth_http::bootstrap_oauth_client(
        node,
        agent_did,
        provider,
        OAuthRefreshKind::ChatGpt,
        CHATGPT_OAUTH_PRODUCT,
    )
    .await?;
    let headers =
        build_chatgpt_codex_headers(credential.account_id.as_deref(), credential.is_fedramp)?;
    let endpoint = normalize_endpoint(endpoint);
    // The capture wrapper sits *below* the Codex wrapper, so it sees the body
    // after `patch_instructions_body` has hoisted `instructions`, stripped
    // system items, set `store`/`stream`, deleted the unsupported sampling
    // params, and forced `strict:false`. Capturing above it would persist a
    // request this backend never receives.
    let http = ChatGptCodexHttpClient::with_inner(
        bearer,
        crate::rendered_request::RenderedRequestCapturingHttpClient::default(),
    );
    crate::inference_http::build_openai_responses_client(
        "chatgpt-oauth-managed",
        &endpoint,
        http,
        headers,
    )
    .context("building ChatGPT Codex Responses client")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Inner transport that yields a fixed HTTP status — `>= 400` becomes the rig status error the
    /// reqwest transport would produce, `< 400` an empty success response.
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
            std::future::ready(Err(http_client::Error::InvalidStatusCode(
                "501".parse().expect("valid status"),
            )))
        }
    }

    fn status_error(status: &str) -> http_client::Error {
        http_client::Error::InvalidStatusCodeWithMessage(
            status.parse().expect("valid status"),
            String::new(),
        )
    }

    /// The reason the capture seam is a transport wrapper at all.
    ///
    /// `build_responses_client` puts the capture client *below* this one so the
    /// durable row is the body ChatGPT actually receives. Nothing about the
    /// types enforces that: both wrappers are generic over their inner client,
    /// so hoisting the capture above the Codex wrapper compiles and streams
    /// fine while persisting a request this backend never sees. This composes
    /// the real stack — Codex over capture over a recording terminal — and
    /// pins the row to the post-rewrite body.
    #[tokio::test]
    async fn the_captured_row_is_the_body_codex_rewrote_not_the_one_rig_serialized() {
        use crate::rendered_request::scope::{arm, scope_request, test_scope, CaptureScopeKind};
        use crate::rendered_request::transport::CountingInner;
        use crate::rendered_request::{
            AssemblyBuildPath, AssemblyTrace, RenderedCompletionRequest,
            RenderedRequestCaptureSink, RenderedRequestContext,
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
                agent_did: "did:key:agent".to_string(),
                requester_did: String::new(),
                behavior_id: "behavior".to_string(),
                session_id: "session".to_string(),
                model_name: "configured-model".to_string(),
            },
            sink,
        );

        // The pre-rewrite body: system text still in `input`, sampling params
        // Codex rejects still present, and a strict tool.
        let assembled = serde_json::json!({
            "model": "gpt-5-codex",
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

    #[test]
    fn bearer_rejection_only_for_401_and_403() {
        use crate::oauth_http::{is_bearer_rejection, IdentityHeaders as _};
        let statuses = ChatGptCodexPolicy::REJECTION_STATUSES;
        assert!(is_bearer_rejection(statuses, &status_error("401")));
        assert!(is_bearer_rejection(statuses, &status_error("403")));
        assert!(!is_bearer_rejection(statuses, &status_error("500")));
        assert!(!is_bearer_rejection(statuses, &status_error("429")));
        assert!(!is_bearer_rejection(
            statuses,
            &http_client::Error::Instance(anyhow::anyhow!("network down").into())
        ));
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

    #[test]
    fn classifies_missing_auth_with_login_guidance() {
        let msg = classify_chatgpt_auth_error(
            "did:key:zAgent",
            CHATGPT_CODEX_PROVIDER,
            &OAuthAuthProblem::Missing,
        );

        assert!(msg.contains("did:key:zAgent"), "names the agent DID: {msg}");
        assert!(
            msg.contains("gents codex-login"),
            "tells the user how to fix it: {msg}"
        );
    }

    #[test]
    fn classifies_wrong_mode_naming_found_mode() {
        let msg = classify_chatgpt_auth_error(
            "did:key:zAgent",
            CHATGPT_CODEX_PROVIDER,
            &OAuthAuthProblem::WrongMode {
                found_mode: "disabled".to_string(),
            },
        );

        assert!(msg.contains("ChatGPT"), "asks for ChatGPT OAuth: {msg}");
        assert!(msg.contains("disabled"), "names what was found: {msg}");
    }

    #[test]
    fn classifies_expired_with_reauth_guidance() {
        let msg = classify_chatgpt_auth_error(
            "did:key:zAgent",
            CHATGPT_CODEX_PROVIDER,
            &OAuthAuthProblem::Expired,
        );

        assert!(msg.to_lowercase().contains("expired"), "{msg}");
        assert!(msg.contains("gents codex-login"), "{msg}");
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
    fn patches_rig_responses_body_for_chatgpt_codex() {
        let body = json!({
            "model": "gpt-5.2",
            "max_output_tokens": 2048,
            "temperature": 0.2,
            "top_p": 0.9,
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [
                        { "type": "input_text", "text": "Use terse answers." }
                    ]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "Say pong." }
                    ]
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "name": "defra_query",
                    "strict": true,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "collection": { "type": "string" },
                            "filter": { "type": "object" }
                        },
                        "required": ["collection", "filter"]
                    }
                },
                {
                    "type": "function",
                    "name": "read_file"
                }
            ]
        });

        let patched = patch_instructions_body(&serde_json::to_vec(&body).unwrap()).unwrap();
        let patched: Value = serde_json::from_slice(&patched).unwrap();

        assert_eq!(
            patched.get("instructions").and_then(Value::as_str),
            Some("Use terse answers.")
        );
        assert_eq!(patched.get("store").and_then(Value::as_bool), Some(false));
        assert_eq!(patched.get("stream").and_then(Value::as_bool), Some(true));
        assert!(patched
            .get("input")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .all(|item| item.get("role").and_then(Value::as_str) != Some("system")));
        for unsupported in CHATGPT_CODEX_UNSUPPORTED_PARAMS {
            assert!(
                patched.get(*unsupported).is_none(),
                "unsupported Codex param {unsupported} should be stripped: {patched}"
            );
        }
        let tools = patched.get("tools").and_then(Value::as_array).unwrap();
        assert!(
            tools
                .iter()
                .all(|tool| tool.get("strict") == Some(&Value::Bool(false))),
            "Codex function tools should match Codex CLI strict:false: {tools:?}"
        );
        assert_eq!(
            tools[0]
                .get("parameters")
                .and_then(|parameters| parameters.get("required"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2),
            "strict:false shaping should not rewrite tool schemas"
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

    // `ensure_event_stream_content_type` is single-owned and unit-tested in
    // `crate::oauth_http` now (`event_stream_content_type_is_added_only_when_missing`).

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
}
