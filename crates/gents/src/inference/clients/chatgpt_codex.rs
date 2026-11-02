use std::future::Future;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use gents_loop::provider_patches::patch_instructions_body;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, Request, ReqwestClient, Response,
};
#[cfg(test)]
use rig::http_client::{MultipartForm, StreamingResponse};
use rig::wasm_compat::WasmCompatSend;
use serde_json::{json, Value};

#[cfg(test)]
use crate::inference::auth::credential::BearerSource;
use crate::inference::auth::credential::{
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
        node_did: impl Into<String>,
        provider: impl Into<String>,
        id_token: &str,
        access_token: String,
        refresh_token: String,
        now: DateTime<Utc>,
    ) -> Self {
        let node_did = node_did.into();
        let provider = provider.into();
        let id_claims = crate::inference::auth::chatgpt_refresh::decode_id_token_claims(id_token);
        let access_token_expires_at =
            crate::inference::auth::chatgpt_refresh::jwt_expiration(&access_token)
                .or(id_claims.expires_at)
                .unwrap_or_else(|| now + Duration::hours(1));
        let provider_account_key = crate::inference::auth::chatgpt_refresh::chatgpt_account_key(
            &access_token,
            Some(id_token),
            id_claims.account_id.as_deref(),
        );
        Self {
            doc_id: None,
            credential_id: oauth_credential_id(&node_did, &provider),
            node_did,
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
            account_ref: None,
            connected_at: None,
            provider_account_key,
            label: None,
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

const CHATGPT_CODEX_CLIENT_VERSION: &str = "0.160.0";
const CHATGPT_CODEX_CLIENT_VERSION_ENV: &str = "GENTS_CHATGPT_CODEX_CLIENT_VERSION";

#[derive(Clone, Default)]
pub struct ChatGptCodexPolicy {
    pub service_tier: Option<crate::inference::resolved::ServiceTier>,
}

impl crate::inference::auth::http::OAuthHttpPolicy for ChatGptCodexPolicy {
    const REJECTION_STATUSES: &'static [u16] = &[401, 403];

    fn patch_request_body(&self, req: Request<Bytes>) -> Request<Bytes> {
        let (mut parts, body) = req.into_parts();
        let mut body = body;
        if parts.uri.path().ends_with("/responses") {
            for name in ["session_id", "x-client-request-id", "x-codex-turn-state"] {
                parts.headers.remove(name);
            }
            if let Some(affinity) = gents_loop::provider_input::routing_affinity::current() {
                if let Ok(session) = HeaderValue::from_str(affinity.session()) {
                    parts.headers.insert("session_id", session.clone());
                    parts.headers.insert("x-client-request-id", session);
                }
                if let Some(mut token) = affinity
                    .token()
                    .and_then(|token| HeaderValue::from_str(token).ok())
                {
                    token.set_sensitive(true);
                    parts.headers.insert("x-codex-turn-state", token);
                }
            }
            if let Some(patched) = patch_instructions_body(&body) {
                body = patched;
            }
            if let Some(patched) = self.service_tier.and_then(|tier| tier.patch(&body).ok()) {
                body = patched.into();
            }
            let call = crate::inference::admission::current_call_join();
            let affinity = gents_loop::provider_input::routing_affinity::current();
            tracing::info!(
                call_id = call.as_ref().map(|call| call.call_id.as_str()),
                call_seq = call.as_ref().map(|call| call.call_seq),
                affinity_scope_present = affinity.is_some(),
                session_header_present = parts.headers.contains_key("session_id"),
                client_request_header_present = parts.headers.contains_key("x-client-request-id"),
                turn_state_header_present = parts.headers.contains_key("x-codex-turn-state"),
                "Codex cache routing request"
            );
        }
        Request::from_parts(parts, body)
    }

    /// The Codex client's streaming completion API dispatches `/responses`.
    /// Other endpoints use buffered sends, which check the URI before capture.
    fn patch_streaming(
        &self,
        response: rig::http_client::StreamingResponse,
    ) -> rig::http_client::StreamingResponse {
        retain_turn_state(response.headers());
        response
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

            if is_responses_request {
                retain_turn_state(response.headers());
            }
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

fn retain_turn_state(headers: &HeaderMap) {
    let affinity = gents_loop::provider_input::routing_affinity::current();
    let returned = headers
        .get("x-codex-turn-state")
        .and_then(|header| header.to_str().ok());
    let retained = affinity.as_ref().and_then(|affinity| affinity.token());
    let call = crate::inference::admission::current_call_join();
    // Opaque routing tokens may carry provider state. Record presence and
    // equality only; body comparisons use the existing rendered captures.
    tracing::info!(
        call_id = call.as_ref().map(|call| call.call_id.as_str()),
        call_seq = call.as_ref().map(|call| call.call_seq),
        provider_request_id = headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        affinity_scope_present = affinity.is_some(),
        turn_state_header_present = headers.contains_key("x-codex-turn-state"),
        turn_state_header_valid = returned.is_some(),
        retained_turn_state_present = retained.is_some(),
        returned_turn_state_matches = retained.zip(returned).map(|(a, b)| a == b),
        "Codex cache routing response"
    );
    if let Some(affinity) = affinity {
        affinity.observe(returned);
    }
}

pub type ChatGptCodexHttpClient<S, H = ReqwestClient> =
    crate::inference::auth::http::BearerAuthHttpClient<S, ChatGptCodexPolicy, H>;

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

pub async fn build_responses_client(
    cache: Arc<crate::inference::usage_observation::UsageCache>,
    node: Arc<EmbeddedNode>,
    node_did: &str,
    account_ref: Option<&str>,
    endpoint: &str,
    service_tier: Option<crate::inference::resolved::ServiceTier>,
) -> Result<
    rig::providers::openai::Client<
        ChatGptCodexHttpClient<
            DbCredentialBearer,
            crate::inference::rendered_request::RenderedRequestCapturingHttpClient<
                crate::inference::clients::provider_http::ProviderHttpClient,
            >,
        >,
    >,
> {
    let provider = CHATGPT_CODEX_PROVIDER;
    let (bearer, credential) = crate::inference::auth::http::bootstrap_oauth_client(
        node.clone(),
        node_did,
        provider,
        OAuthRefreshKind::ChatGpt,
        CHATGPT_OAUTH_PRODUCT,
        crate::inference::auth::credential::AccountPick::Reference(account_ref),
    )
    .await?;
    let headers =
        build_chatgpt_codex_headers(credential.account_id.as_deref(), credential.is_fedramp)?;
    let endpoint = normalize_endpoint(endpoint);
    let usage = crate::inference::usage_observation::UsageReporter::new(
        cache,
        node,
        crate::inference::usage_observation::UsageAccount::for_credential(&credential),
    );
    let http = ChatGptCodexHttpClient::with_inner(
        bearer,
        crate::inference::rendered_request::RenderedRequestCapturingHttpClient::new(
            crate::inference::clients::provider_http::ProviderHttpClient::with_usage(
                Default::default(),
                usage,
            ),
        ),
    )
    .with_policy(ChatGptCodexPolicy { service_tier });
    crate::inference::clients::inference_http::build_openai_responses_client(
        "chatgpt-oauth-managed",
        &endpoint,
        http,
        headers,
    )
    .context("building ChatGPT Codex Responses client")
}

#[cfg(test)]
mod usage_tests;

#[cfg(test)]
mod tests;
