use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use tracing::Instrument;

use crate::document_config::AdvertisedModel;

// The enum itself (and its pure parse/as_str/is_agent_scoped_oauth/Display)
// moved to gents-loop (G-1): the loop's provider_input layer switches on it
// with no reqwest dependency. This crate re-exports it and adds the two
// OAuth-guidance methods below through an extension trait, since only the
// type's defining crate may add inherent methods to it.
pub use gents_loop::backend_provider::BackendProviderKind;

/// OAuth-credential guidance for a backend provider kind: which
/// `OAuthCredential.provider` it authenticates with, and how to render an
/// auth problem for it. Kept here (not on the moved enum) because both touch
/// native, agent-scoped OAuth modules the guest never links.
pub(crate) trait BackendProviderOauthExt {
    fn oauth_provider(self) -> Option<&'static str>;
    fn oauth_auth_guidance(
        self,
        agent_did: &str,
        provider: &str,
        problem: &crate::oauth_credential::OAuthAuthProblem,
    ) -> String;
}

impl BackendProviderOauthExt for BackendProviderKind {
    /// The `OAuthCredential.provider` value an agent-scoped kind authenticates with.
    fn oauth_provider(self) -> Option<&'static str> {
        match self {
            Self::ChatGptCodex => Some(crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER),
            Self::XaiGrokOAuth => Some(crate::xai_grok_oauth::XAI_OAUTH_PROVIDER),
            Self::ClaudeCliSubscription => Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
            _ => None,
        }
    }

    fn oauth_auth_guidance(
        self,
        agent_did: &str,
        provider: &str,
        problem: &crate::oauth_credential::OAuthAuthProblem,
    ) -> String {
        match self {
            Self::ChatGptCodex => {
                crate::oauth_credential::classify_chatgpt_auth_error(agent_did, provider, problem)
            }
            Self::XaiGrokOAuth => {
                crate::xai_grok_oauth::classify_xai_auth_error(agent_did, provider, problem)
            }
            Self::ClaudeCliSubscription => {
                crate::claude_oauth::classify_claude_auth_error(agent_did, provider, problem)
            }
            _ => format!("{self} does not use OAuth credentials"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{provider} model discovery failed at {url}: {status} {body}")]
pub struct ModelDiscoveryHttpError {
    pub provider: String,
    pub url: String,
    pub status: u16,
    pub body: String,
}

impl ModelDiscoveryHttpError {
    pub fn is_auth(&self) -> bool {
        matches!(self.status, 401 | 403)
    }
}

fn provider_display_name(kind: BackendProviderKind) -> &'static str {
    match kind {
        BackendProviderKind::OpenAiCompatible => "OpenAI-compatible",
        BackendProviderKind::OpenRouter => "OpenRouter",
        BackendProviderKind::ChatGptCodex => "ChatGPT Codex",
        BackendProviderKind::XaiGrokOAuth => "Grok / xAI OAuth",
        BackendProviderKind::ClaudeCliSubscription => "Claude CLI subscription",
    }
}

const MODEL_DISCOVERY_PATH: &str = "/models";
/// The Grok CLI proxy publishes its catalog at `/models-v2` (official client path).
const XAI_GROK_MODEL_DISCOVERY_PATH: &str = "/models-v2";
/// Claude subscription catalog base. The backend document's endpoint is the
/// `claude-cli://subscription` placeholder; an `http(s)://` endpoint (tests)
/// overrides this.
const CLAUDE_MODELS_BASE: &str = "https://api.anthropic.com/v1";

fn claude_models_base(endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.trim_end_matches('/').to_string()
    } else {
        CLAUDE_MODELS_BASE.to_string()
    }
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModelRecord>,
    #[serde(default)]
    models: Vec<ChatGptCodexModelRecord>,
}

#[derive(Deserialize)]
struct OpenAiModelRecord {
    id: Option<String>,
    model: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    /// Grok `/models-v2` catalog rows advertise a display name and context
    /// window per model; the OpenAI-style catalogs observed by the existing
    /// adapter do not, so these stay `None` elsewhere.
    name: Option<String>,
    #[serde(rename = "contextWindow")]
    context_window: Option<i64>,
}

impl OpenAiModelRecord {
    /// OpenAI-style catalogs identify models by `id`; the Grok `/models-v2`
    /// catalog identifies them by `model` / `modelId` (`id` is a row id there).
    fn identifier(self, kind: BackendProviderKind) -> Option<String> {
        let ordered = if kind == BackendProviderKind::XaiGrokOAuth {
            [self.model, self.model_id, self.id]
        } else {
            [self.id, self.model, self.model_id]
        };
        ordered
            .into_iter()
            .flatten()
            .find(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
    }

    /// Canonical advertised record for one catalog row. Capabilities stay
    /// `None` (unknown) except where the provider's catalog shape — per the
    /// existing adapter evidence — advertises them: only Grok `/models-v2`
    /// rows carry `name` / `contextWindow`. No supported efforts, output
    /// tokens, or model profiles are invented here.
    fn into_advertised(self, kind: BackendProviderKind) -> Option<AdvertisedModel> {
        let (name, context_window) = (
            if kind == BackendProviderKind::XaiGrokOAuth {
                self.name.clone()
            } else {
                None
            },
            if kind == BackendProviderKind::XaiGrokOAuth {
                self.context_window
            } else {
                None
            },
        );
        let model_name = self.identifier(kind)?;
        let display_name = name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && value != &model_name);
        Some(AdvertisedModel {
            model_name,
            display_name,
            context_window,
            max_output_tokens: None,
            reasoning_efforts: None,
        })
    }
}

#[derive(Deserialize)]
struct ChatGptCodexModelRecord {
    slug: Option<String>,
    id: Option<String>,
    name: Option<String>,
    model: Option<String>,
}

impl ChatGptCodexModelRecord {
    fn identifier(self) -> Option<String> {
        [self.slug, self.id, self.name, self.model]
            .into_iter()
            .flatten()
            .find(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
    }

    /// The Codex `models` shape advertises identifiers only. `name` participates
    /// in identifier fallback, so it is not promoted to a display name; all
    /// capabilities stay unknown.
    fn into_advertised(self) -> Option<AdvertisedModel> {
        let model_name = self.identifier()?;
        Some(AdvertisedModel {
            model_name,
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            reasoning_efforts: None,
        })
    }
}

pub async fn discover_models(
    client: &Client,
    kind: BackendProviderKind,
    endpoint: &str,
    api_key: Option<&str>,
    oauth_credential: Option<&crate::oauth_credential::OAuthCredential>,
) -> Result<Vec<AdvertisedModel>> {
    let endpoint = match kind {
        BackendProviderKind::ChatGptCodex => crate::chatgpt_codex::normalize_endpoint(endpoint),
        BackendProviderKind::XaiGrokOAuth => crate::xai_grok_oauth::normalize_endpoint(endpoint),
        BackendProviderKind::ClaudeCliSubscription => claude_models_base(endpoint),
        _ => endpoint.trim_end_matches('/').to_string(),
    };
    let discovery_path = if kind == BackendProviderKind::XaiGrokOAuth {
        XAI_GROK_MODEL_DISCOVERY_PATH
    } else {
        MODEL_DISCOVERY_PATH
    };
    let models_url = format!("{endpoint}{discovery_path}");
    let provider_name = provider_display_name(kind);
    async {
        let mut request = client.get(&models_url);
        if kind == BackendProviderKind::ChatGptCodex {
            let Some(credential) = oauth_credential else {
                tracing::Span::current().record("failure_class", "auth");
                anyhow::bail!(
                    "ChatGPT Codex model discovery requires an OAuthCredential document; run `gents codex-login` for the agent DID first"
                );
            };
            request = request.bearer_auth(&credential.access_token);
            let headers = match crate::chatgpt_codex::build_chatgpt_codex_headers(
                credential.account_id.as_deref(),
                credential.is_fedramp,
            ) {
                Ok(headers) => headers,
                Err(error) => {
                    tracing::Span::current().record("failure_class", "auth");
                    return Err(error);
                }
            };
            for (name, value) in headers {
                if let Some(name) = name {
                    request = request.header(name, value);
                }
            }
            request = request.query(&[(
                "client_version",
                crate::chatgpt_codex::chatgpt_codex_client_version(),
            )]);
        } else if kind == BackendProviderKind::XaiGrokOAuth {
            let Some(credential) = oauth_credential else {
                tracing::Span::current().record("failure_class", "auth");
                anyhow::bail!(
                    "Grok OAuth model discovery requires an OAuthCredential document; run `gents grok-login` for the agent DID first"
                );
            };
            request = request.bearer_auth(&credential.access_token);
            let headers = match crate::xai_grok_oauth::build_xai_grok_oauth_headers() {
                Ok(headers) => headers,
                Err(error) => {
                    tracing::Span::current().record("failure_class", "auth");
                    return Err(error);
                }
            };
            for (name, value) in headers {
                if let Some(name) = name {
                    request = request.header(name, value);
                }
            }
        } else if kind == BackendProviderKind::ClaudeCliSubscription {
            let Some(credential) = oauth_credential else {
                tracing::Span::current().record("failure_class", "auth");
                anyhow::bail!(
                    "Claude subscription model discovery requires an OAuthCredential document; run `gents claude-login --agent-did <did>` for the agent DID first"
                );
            };
            request = request
                .bearer_auth(&credential.access_token)
                .header(
                    "anthropic-version",
                    crate::claude_messages::ANTHROPIC_VERSION,
                )
                .header("anthropic-beta", crate::claude_messages::OAUTH_BETA)
                .query(&[("limit", "100")]);
        } else if let Some(api_key) = api_key {
            request = request.bearer_auth(api_key);
        }
        for (name, value) in crate::runtime_trace::current_trace_context_headers() {
            let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
                continue;
            };
            let Ok(value) = reqwest::header::HeaderValue::from_str(&value) else {
                continue;
            };
            request = request.header(name, value);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::Span::current().record("failure_class", "transport");
                return Err(error).with_context(|| {
                    format!("querying {provider_name} models endpoint {models_url}")
                });
            }
        };
        let status = response.status();
        tracing::Span::current().record("http_status", status.as_u16() as i64);
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable body>".to_string());
        if !status.is_success() {
            tracing::Span::current().record("failure_class", "http_status");
            return Err(ModelDiscoveryHttpError {
                provider: provider_name.to_string(),
                url: models_url.to_string(),
                status: status.as_u16(),
                body: truncate_probe_body(&body),
            }
            .into());
        }

        let models: OpenAiModelsResponse = match serde_json::from_str(&body) {
            Ok(models) => models,
            Err(error) => {
                tracing::Span::current().record("failure_class", "decode");
                return Err(error).with_context(|| {
                    format!(
                        "decoding {} model discovery response from {}: {}",
                        provider_name,
                        models_url,
                        truncate_probe_body(&body)
                    )
                });
            }
        };

        let openai_models = models
            .data
            .into_iter()
            .filter_map(|model| model.into_advertised(kind));
        let chatgpt_codex_models = models
            .models
            .into_iter()
            .filter_map(ChatGptCodexModelRecord::into_advertised);
        let mut models = Vec::new();
        for model in openai_models.chain(chatgpt_codex_models) {
            if !models.iter().any(|advertised: &AdvertisedModel| {
                advertised.model_name == model.model_name
            }) {
                models.push(model);
            }
        }
        tracing::Span::current().record("model_count", models.len() as i64);
        Ok(models)
    }
    .instrument(tracing::info_span!(
        "backend.model_discovery",
        provider_kind = %kind,
        endpoint = %endpoint,
        has_api_key = api_key.is_some(),
        http_status = tracing::field::Empty,
        model_count = tracing::field::Empty,
        failure_class = tracing::field::Empty,
    ))
    .await
}

pub fn truncate_probe_body(body: &str) -> String {
    const LIMIT: usize = 256;
    if body.len() <= LIMIT {
        return body.to_string();
    }
    let mut end = LIMIT;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &body[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_error_body_truncation_preserves_utf8_boundaries() {
        assert_eq!(truncate_probe_body("short error"), "short error");
        for character in ['é', '界', '🦀'] {
            let prefix = "x".repeat(255);
            let body = format!("{prefix}{character}backend error");
            assert_eq!(truncate_probe_body(&body), format!("{prefix}..."));
        }
        assert_eq!(truncate_probe_body(&"x".repeat(256)), "x".repeat(256));
    }

    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    fn model_names(models: &[AdvertisedModel]) -> Vec<&str> {
        models
            .iter()
            .map(|model| model.model_name.as_str())
            .collect()
    }

    #[tokio::test]
    async fn discover_models_reads_openai_models_and_sends_api_key() {
        let (endpoint, requests) =
            spawn_model_discovery_server(r#"{"data":[{"id":"gpt-4.1-mini"},{"id":" o3 "}]}"#).await;

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::OpenAiCompatible,
            &format!("{endpoint}/v1/"),
            Some("sk-test"),
            None,
        )
        .await
        .expect("model discovery should succeed");

        assert_eq!(model_names(&models), vec!["gpt-4.1-mini", "o3"]);
        let requests = requests.lock().expect("requests lock");
        let request = requests.first().expect("captured request");
        assert!(
            request.starts_with("GET /v1/models "),
            "unexpected request: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-test"),
            "authorization header missing from {request}"
        );
    }

    #[tokio::test]
    async fn discover_models_decodes_chatgpt_codex_models_shape() {
        let (endpoint, _requests) =
            spawn_model_discovery_server(r#"{"models":[{"slug":" codex-mini-latest "}]}"#).await;

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::OpenAiCompatible,
            &endpoint,
            None,
            None,
        )
        .await
        .expect("model discovery should accept the Codex-compatible models shape");

        assert_eq!(model_names(&models), vec!["codex-mini-latest"]);
    }

    #[tokio::test]
    async fn discover_models_accepts_llama_models_response_with_non_slug_models() {
        let (endpoint, _requests) = spawn_model_discovery_server(
            r#"{"object":"list","data":[{"id":"google/gemma-4-12B-it-qat-q4_0-gguf","object":"model"}],"models":[{"name":"google/gemma-4-12B-it-qat-q4_0-gguf","model":"google/gemma-4-12B-it-qat-q4_0-gguf","modified_at":1710000000}]}"#,
        )
        .await;

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::OpenAiCompatible,
            &endpoint,
            None,
            None,
        )
        .await
        .expect("model discovery should accept llama.cpp OpenAI-compatible models shape");

        assert_eq!(
            model_names(&models),
            vec!["google/gemma-4-12B-it-qat-q4_0-gguf"]
        );
    }

    #[tokio::test]
    async fn discover_models_accepts_common_models_id_name_model_fields() {
        let (endpoint, _requests) = spawn_model_discovery_server(
            r#"{"models":[{"id":"from-id"},{"name":"from-name"},{"model":"from-model"}]}"#,
        )
        .await;

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::OpenAiCompatible,
            &endpoint,
            None,
            None,
        )
        .await
        .expect("model discovery should accept common non-Codex models fields");

        assert_eq!(
            model_names(&models),
            vec!["from-id", "from-name", "from-model"]
        );
    }

    #[tokio::test]
    async fn discover_models_reads_grok_models_v2_shape() {
        // Official Grok CLI catalog shape: `/models-v2` returns `{"data":[...]}`
        // where the model identifier is `model` / `modelId`, not `id` (which is
        // a catalog row id when present).
        let (endpoint, requests) = spawn_model_discovery_server(
            r#"{"data":[{"id":"row-1","model":"grok-4.5","name":"Grok 4.5","contextWindow":256000,"apiBackend":"responses"},{"id":"row-2","modelId":"grok-build-0.1","name":"Grok Build"}]}"#,
        )
        .await;
        let credential = crate::oauth_credential::OAuthCredential {
            doc_id: None,
            credential_id: "xai-oauth:did:key:zAgent".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            provider: crate::xai_grok_oauth::XAI_OAUTH_PROVIDER.to_string(),
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
        };

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::XaiGrokOAuth,
            &endpoint,
            None,
            Some(&credential),
        )
        .await
        .expect("Grok OAuth model discovery should accept the /models-v2 shape");

        assert_eq!(model_names(&models), vec!["grok-4.5", "grok-build-0.1"]);
        let requests = requests.lock().expect("requests lock");
        assert!(
            requests[0].starts_with("GET /models-v2"),
            "Grok discovery must query the official /models-v2 catalog: {}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn discover_models_sends_chatgpt_codex_version_header_and_query_param() {
        let (endpoint, requests) =
            spawn_model_discovery_server(r#"{"models":[{"slug":"gpt-5.5"}]}"#).await;
        let credential = crate::oauth_credential::OAuthCredential {
            doc_id: None,
            credential_id: "chatgpt-codex:did:key:zAgent".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            provider: crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER.to_string(),
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            id_token: None,
            account_id: Some("acct_123".to_string()),
            chatgpt_plan_type: Some("plus".to_string()),
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
        };

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::ChatGptCodex,
            &endpoint,
            None,
            Some(&credential),
        )
        .await
        .expect("ChatGPT Codex model discovery should succeed");

        assert_eq!(model_names(&models), vec!["gpt-5.5"]);
        let requests = requests.lock().expect("requests lock");
        let request = requests.first().expect("captured request");
        let version = crate::chatgpt_codex::chatgpt_codex_client_version();
        assert!(
            request.starts_with(&format!("GET /models?client_version={version} ")),
            "Codex /models should advertise client_version query param: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains(&format!("version: {}", version).to_ascii_lowercase()),
            "Codex /models should advertise matching version header: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("accept: text/event-stream, application/json"),
            "Codex /models should send Codex Accept header: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer access-token"),
            "Codex /models should send OAuth bearer: {request}"
        );
    }

    fn claude_credential() -> crate::oauth_credential::OAuthCredential {
        crate::oauth_credential::OAuthCredential {
            doc_id: None,
            credential_id: "claude-subscription:did:key:zAgent".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            provider: crate::claude_oauth::CLAUDE_OAUTH_PROVIDER.to_string(),
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn discover_models_reads_claude_models_with_oauth_headers() {
        // Live probe: `GET https://api.anthropic.com/v1/models` with the
        // subscription bearer returns the OpenAI-style `{"data":[{"id":...}]}`.
        let (endpoint, requests) = spawn_model_discovery_server(
            r#"{"data":[{"id":"claude-fable-5-1","type":"model"},{"id":"claude-opus-5","type":"model"},{"id":"claude-sonnet-5","type":"model"}],"has_more":false}"#,
        )
        .await;
        let credential = claude_credential();

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::ClaudeCliSubscription,
            &format!("{endpoint}/v1"),
            None,
            Some(&credential),
        )
        .await
        .expect("Claude subscription model discovery should read /v1/models");

        assert_eq!(
            model_names(&models),
            vec!["claude-fable-5-1", "claude-opus-5", "claude-sonnet-5"]
        );
        let requests = requests.lock().expect("requests lock");
        let request = requests
            .first()
            .expect("captured request")
            .to_ascii_lowercase();
        assert!(
            request.starts_with("get /v1/models?limit=100 "),
            "Claude discovery must query /v1/models?limit=100: {request}"
        );
        for header in [
            "authorization: bearer access-token",
            "anthropic-version: 2023-06-01",
            "anthropic-beta: oauth-2025-04-20",
        ] {
            assert!(request.contains(header), "{header} missing from {request}");
        }
    }

    #[tokio::test]
    async fn discover_models_claude_401_is_an_auth_error() {
        let (endpoint, _requests) = spawn_model_discovery_server_with_status(
            "401 Unauthorized",
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid bearer"}}"#,
        )
        .await;
        let credential = claude_credential();

        let error = discover_models(
            &Client::new(),
            BackendProviderKind::ClaudeCliSubscription,
            &format!("{endpoint}/v1"),
            None,
            Some(&credential),
        )
        .await
        .expect_err("401 from /v1/models must fail discovery");

        let http = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ModelDiscoveryHttpError>())
            .expect("401 must surface as ModelDiscoveryHttpError so the CLI appends claude-login guidance");
        assert!(http.is_auth(), "{http}");
    }

    #[tokio::test]
    async fn discover_models_claude_without_credential_names_claude_login() {
        let error = discover_models(
            &Client::new(),
            BackendProviderKind::ClaudeCliSubscription,
            "claude-cli://subscription",
            None,
            None,
        )
        .await
        .expect_err("Claude discovery without a credential must fail");

        let message = format!("{error:#}");
        assert!(
            message.contains("requires an OAuthCredential document")
                && message.contains("--agent-did"),
            "missing-credential error must name the login command: {message}"
        );
    }

    #[test]
    fn claude_models_base_ignores_the_placeholder_endpoint() {
        assert_eq!(
            claude_models_base("claude-cli://subscription"),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            claude_models_base("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080/v1"
        );
    }

    async fn spawn_model_discovery_server(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        spawn_model_discovery_server_with_status("200 OK", body).await
    }

    async fn spawn_model_discovery_server_with_status(
        status: &'static str,
        body: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock model-discovery server");
        let addr = listener
            .local_addr()
            .expect("mock model-discovery server address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captures = Arc::clone(&requests);

        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(request) = read_http_request(&mut stream).await else {
                return;
            };
            captures
                .lock()
                .expect("requests lock")
                .push(request.to_string());
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        (format!("http://{addr}"), requests)
    }

    async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<String> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            buf.extend_from_slice(&chunk[..n]);
            if find_bytes(&buf, b"\r\n\r\n").is_some() {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
