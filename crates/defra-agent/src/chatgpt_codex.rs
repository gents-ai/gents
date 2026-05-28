use std::future::Future;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
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

pub async fn load_chatgpt_auth(codex_home: PathBuf) -> Result<CodexAuth> {
    let auth_manager = AuthManager::new(
        codex_home.clone(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::Auto,
        /*chatgpt_base_url*/ None,
    )
    .await;

    // `auth()` follows Codex behavior and may refresh near-expiry managed tokens.
    let auth = auth_manager
        .auth()
        .await
        .with_context(|| format!("no Codex auth found in {}", codex_home.display()))?;

    if !auth.is_chatgpt_auth() {
        bail!(
            "expected ChatGPT OAuth auth in {}, found {:?}",
            codex_home.display(),
            auth.auth_mode()
        );
    }

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

#[derive(Clone, Debug, Default)]
pub struct ChatGptCodexHttpClient {
    inner: ReqwestClient,
}

impl ChatGptCodexHttpClient {
    fn inject_required_instructions<T>(req: Request<T>) -> Request<Bytes>
    where
        T: Into<Bytes>,
    {
        let (parts, body) = req.into_parts();
        let mut body = body.into();
        if parts.uri.path().ends_with("/responses") {
            if let Some(patched) = patch_instructions_body(&body) {
                body = patched;
            }
        }
        Request::from_parts(parts, body)
    }
}

impl HttpClientExt for ChatGptCodexHttpClient {
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
        let req = Self::inject_required_instructions(req);
        async move { send_reqwest(inner, req).await }
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
        async move { HttpClientExt::send_multipart(&inner, req).await }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.inner.clone();
        let req = Self::inject_required_instructions(req);
        async move { HttpClientExt::send_streaming(&inner, req).await }
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
        Value::Object(item) => {
            if item.get("role").and_then(Value::as_str) == Some("system") {
                item.clear();
            }
        }
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
) -> Result<rig::providers::openai::Client<ChatGptCodexHttpClient>> {
    let (codex_home, auth) = load_default_chatgpt_auth().await?;
    let access_token = auth.get_token().with_context(|| {
        format!(
            "ChatGPT auth in {} did not expose a bearer token",
            codex_home.display()
        )
    })?;
    let headers = build_chatgpt_codex_headers(&auth)?;
    rig::providers::openai::Client::builder()
        .api_key(access_token)
        .base_url(normalize_endpoint(endpoint))
        .http_headers(headers)
        .http_client(ChatGptCodexHttpClient::default())
        .build()
        .context("building ChatGPT Codex Responses client")
}

#[cfg(test)]
mod tests {
    use super::*;

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
