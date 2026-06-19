use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fmt, fmt::Formatter};

use anyhow::{Context, Result};
use bytes::Bytes;
use codex_login::{
    default_client::default_headers, AuthCredentialsStoreMode, AuthManager, CodexAuth,
};
use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient,
    Response, StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;
use serde_json::{json, Value};

pub const DEFRA_CODEX_HOME_ENV: &str = "DEFRA_CODEX_HOME";

pub fn default_backend_endpoint() -> &'static str {
    CHATGPT_CODEX_BASE_URL
}

pub fn resolve_codex_home(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Ok(path) = std::env::var(DEFRA_CODEX_HOME_ENV) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    dirs::home_dir()
        .map(|home| home.join(".codex"))
        .context("could not determine home directory for default ~/.codex")
}

/// A user-actionable classification of why ChatGPT OAuth could not be used.
#[derive(Debug, Clone)]
pub enum ChatGptAuthProblem {
    /// No Codex credentials found in the resolved home.
    Missing,
    /// Credentials exist but are not ChatGPT OAuth (for example, an API key).
    ///
    /// `found_mode` is the stringified Codex auth mode. Keeping it local avoids
    /// adding a direct dependency just to expose that transitive enum.
    WrongMode { found_mode: String },
    /// Credentials are ChatGPT OAuth but the token is expired or revoked.
    Expired,
    /// Anything else, with the underlying message.
    Other(String),
}

/// Render an actionable, multi-line message for a ChatGPT auth failure.
pub fn classify_chatgpt_auth_error(codex_home: &Path, problem: &ChatGptAuthProblem) -> String {
    let home = codex_home.display();
    match problem {
        ChatGptAuthProblem::Missing => format!(
            "No Codex credentials found in {home}.\n\
             To use the ChatGPT subscription backend, sign in with the Codex CLI \
             (`codex login`), or point DEFRA_CODEX_HOME at a home that already has \
             ChatGPT OAuth credentials."
        ),
        ChatGptAuthProblem::WrongMode { found_mode } => format!(
            "Credentials in {home} are {found_mode}, but the ChatGPT subscription \
             backend needs ChatGPT OAuth.\n\
             Run `codex login` to establish a ChatGPT session, or select an \
             API-key backend instead."
        ),
        ChatGptAuthProblem::Expired => format!(
            "ChatGPT OAuth credentials in {home} are expired or revoked.\n\
             Re-authenticate with `codex login` to refresh the session."
        ),
        ChatGptAuthProblem::Other(detail) => {
            format!("ChatGPT auth in {home} could not be used: {detail}")
        }
    }
}

/// Build an AuthManager for `codex_home` and resolve usable ChatGPT OAuth,
/// classifying the failure precisely so callers can give actionable guidance.
///
/// `AuthManager::auth()` may proactively refresh and persist the managed token
/// using Codex's normal behavior, so this is not a read-only operation.
pub async fn resolve_chatgpt_auth(
    codex_home: &Path,
) -> std::result::Result<(Arc<AuthManager>, CodexAuth), ChatGptAuthProblem> {
    let manager = Arc::new(
        AuthManager::new(
            codex_home.to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::Auto,
            /*chatgpt_base_url*/ None,
        )
        .await,
    );
    // `auth()` attempts a proactive refresh. On permanent refresh failure it
    // logs and returns the stale auth, so ask the manager for that failure too.
    let auth = manager.auth().await.ok_or(ChatGptAuthProblem::Missing)?;
    if !auth.is_chatgpt_auth() {
        return Err(ChatGptAuthProblem::WrongMode {
            found_mode: format!("{:?}", auth.auth_mode()),
        });
    }
    if manager.refresh_failure_for_auth(&auth).is_some() {
        return Err(ChatGptAuthProblem::Expired);
    }
    Ok((manager, auth))
}

pub async fn load_chatgpt_auth(codex_home: PathBuf) -> Result<CodexAuth> {
    let (_, auth) = resolve_chatgpt_auth(&codex_home)
        .await
        .map_err(|problem| anyhow::anyhow!(classify_chatgpt_auth_error(&codex_home, &problem)))?;
    Ok(auth)
}

pub async fn load_default_chatgpt_auth() -> Result<(PathBuf, CodexAuth)> {
    let codex_home = resolve_codex_home(None)?;
    let auth = load_chatgpt_auth(codex_home.clone()).await?;
    Ok((codex_home, auth))
}

pub fn normalize_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default_backend_endpoint().to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn build_chatgpt_codex_headers(auth: &CodexAuth) -> Result<HeaderMap> {
    let mut headers = default_headers();
    if let Some(account_id) = auth.get_account_id() {
        let account_id = HeaderValue::from_str(&account_id)
            .context("ChatGPT account id could not be encoded as an HTTP header")?;
        headers.insert("ChatGPT-Account-ID", account_id);
    }
    if auth.is_fedramp_account() {
        headers.insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
    }
    headers.insert(
        "version",
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    Ok(headers)
}

/// Supplies a current OAuth bearer, refreshing it as needed.
pub trait BearerSource: Send + Sync {
    fn current_bearer(&self) -> impl Future<Output = Result<String>> + Send;
}

/// Production [`BearerSource`] backed by Codex's [`AuthManager`], whose `auth()`
/// proactively refreshes a near-expiry managed token before returning it.
#[derive(Clone)]
pub struct AuthManagerBearer(pub Arc<AuthManager>);

impl BearerSource for AuthManagerBearer {
    async fn current_bearer(&self) -> Result<String> {
        let auth = self
            .0
            .auth()
            .await
            .context("no Codex ChatGPT auth available")?;
        if !auth.is_chatgpt_auth() {
            anyhow::bail!(
                "Codex auth changed to {:?}; ChatGPT OAuth is required",
                auth.auth_mode()
            );
        }
        if self.0.refresh_failure_for_auth(&auth).is_some() {
            anyhow::bail!("ChatGPT OAuth credentials are expired or revoked");
        }
        auth.get_token()
            .context("ChatGPT auth did not expose a bearer token")
    }
}

pub struct ChatGptCodexHttpClient<S: BearerSource> {
    inner: ReqwestClient,
    bearer: Option<Arc<S>>,
}

impl<S: BearerSource> Clone for ChatGptCodexHttpClient<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bearer: self.bearer.clone(),
        }
    }
}

impl<S: BearerSource> fmt::Debug for ChatGptCodexHttpClient<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatGptCodexHttpClient")
            .field("inner", &self.inner)
            .field("bearer_configured", &self.bearer.is_some())
            .finish()
    }
}

impl<S: BearerSource> Default for ChatGptCodexHttpClient<S> {
    fn default() -> Self {
        Self {
            inner: ReqwestClient::default(),
            bearer: None,
        }
    }
}

impl<S: BearerSource> ChatGptCodexHttpClient<S> {
    pub fn new(bearer: Arc<S>) -> Self {
        Self {
            inner: ReqwestClient::default(),
            bearer: Some(bearer),
        }
    }

    async fn fresh_auth_header(&self) -> http_client::Result<HeaderValue> {
        let bearer = self.bearer.as_ref().ok_or_else(|| {
            http_client::Error::Instance(
                anyhow::anyhow!("ChatGptCodexHttpClient used without a configured BearerSource")
                    .into(),
            )
        })?;
        let token = bearer
            .current_bearer()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?;
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| http_client::Error::Instance(anyhow::Error::from(error).into()))
    }

    async fn prepare(&self, req: Request<Bytes>) -> http_client::Result<Request<Bytes>> {
        let req = Self::inject_required_instructions(req);
        let value = self.fresh_auth_header().await?;
        let (mut parts, body) = req.into_parts();
        parts.headers.insert("authorization", value);
        Ok(Request::from_parts(parts, body))
    }

    fn inject_required_instructions(req: Request<Bytes>) -> Request<Bytes> {
        let (parts, body) = req.into_parts();
        let mut body = body;
        if parts.uri.path().ends_with("/responses") {
            if let Some(patched) = patch_instructions_body(&body) {
                body = patched;
            }
        }
        Request::from_parts(parts, body)
    }

    #[cfg(test)]
    pub async fn prepare_for_test(
        &self,
        req: Request<Bytes>,
    ) -> http_client::Result<Request<Bytes>> {
        self.prepare(req).await
    }
}

impl<S: BearerSource + 'static> HttpClientExt for ChatGptCodexHttpClient<S> {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            send_reqwest(inner, req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        async move {
            let value = this.fresh_auth_header().await?;
            let (mut parts, body) = req.into_parts();
            parts.headers.insert("authorization", value);
            let req = Request::from_parts(parts, body);
            HttpClientExt::send_multipart(&inner, req).await
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            HttpClientExt::send_streaming(&inner, req).await
        }
    }
}

async fn send_reqwest<U>(
    inner: ReqwestClient,
    req: Request<Bytes>,
) -> http_client::Result<Response<LazyBody<U>>>
where
    U: From<Bytes>,
    U: WasmCompatSend + 'static,
{
    let is_responses_request = req.uri().path().ends_with("/responses");
    let request_body = req.body().clone();
    let (parts, body) = req.into_parts();
    let response = inner
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .map_err(|error| http_client::Error::Instance(error.into()))?;

    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        return Err(http_client::Error::InvalidStatusCodeWithMessage(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    let body = if is_responses_request {
        let text = response
            .text()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?;
        synthesize_completion_response(&request_body, &text)
    } else {
        response
            .bytes()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?
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

fn patch_instructions_body(body: &[u8]) -> Option<Bytes> {
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
    if !changed {
        return None;
    }
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

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
        "id": "defra-chatgpt-codex-response",
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
                "id": "defra-chatgpt-codex-message",
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
    endpoint: &str,
) -> Result<rig::providers::openai::Client<ChatGptCodexHttpClient<AuthManagerBearer>>> {
    let codex_home = resolve_codex_home(None)?;
    let (manager, auth) = resolve_chatgpt_auth(&codex_home)
        .await
        .map_err(|problem| anyhow::anyhow!(classify_chatgpt_auth_error(&codex_home, &problem)))?;
    let headers = build_chatgpt_codex_headers(&auth)?;
    let endpoint = normalize_endpoint(endpoint);
    let http = ChatGptCodexHttpClient::new(Arc::new(AuthManagerBearer(manager)));
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
    }

    impl BearerSource for CountingBearer {
        async fn current_bearer(&self) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.token.clone())
        }
    }

    #[test]
    fn classifies_missing_auth_with_login_guidance() {
        let home = Path::new("/tmp/codex-home");
        let msg = classify_chatgpt_auth_error(home, &ChatGptAuthProblem::Missing);

        assert!(msg.contains("/tmp/codex-home"), "names the home: {msg}");
        assert!(
            msg.contains("codex login"),
            "tells the user how to fix it: {msg}"
        );
    }

    #[test]
    fn classifies_wrong_mode_naming_found_mode() {
        let home = Path::new("/tmp/codex-home");
        let msg = classify_chatgpt_auth_error(
            home,
            &ChatGptAuthProblem::WrongMode {
                found_mode: "ApiKey".to_string(),
            },
        );

        assert!(msg.contains("ChatGPT"), "asks for ChatGPT OAuth: {msg}");
        assert!(msg.contains("ApiKey"), "names what was found: {msg}");
    }

    #[test]
    fn classifies_expired_with_reauth_guidance() {
        let home = Path::new("/tmp/codex-home");
        let msg = classify_chatgpt_auth_error(home, &ChatGptAuthProblem::Expired);

        assert!(msg.to_lowercase().contains("expired"), "{msg}");
        assert!(msg.contains("codex login"), "{msg}");
    }

    #[tokio::test]
    async fn injects_fresh_bearer_on_each_request() {
        let bearer = Arc::new(CountingBearer {
            token: "tok-123".to_string(),
            calls: AtomicUsize::new(0),
        });
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
    }

    #[test]
    fn streamed_output_prefers_deltas_over_done_text() {
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"pong\"}\n",
            "\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"text\":\"pong\"}\n",
            "\n"
        );

        assert_eq!(streamed_output_text(sse), "pong");
    }
}
