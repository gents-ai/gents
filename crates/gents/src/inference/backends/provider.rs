use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use tracing::Instrument;

use crate::documents::AdvertisedModel;

pub use gents_loop::backend_provider::BackendProviderKind;

pub trait BackendProviderOauthExt {
    fn oauth_provider(self) -> Option<&'static str>;
}

impl BackendProviderOauthExt for BackendProviderKind {
    fn oauth_provider(self) -> Option<&'static str> {
        match self {
            Self::ChatGptCodex => {
                Some(crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER)
            }
            Self::XaiGrokOAuth => Some(crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER),
            Self::ClaudeCliSubscription => {
                Some(crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER)
            }
            _ => None,
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

    pub fn is_client_version_gate(&self) -> bool {
        self.status == 426
    }
}

fn provider_display_name(kind: BackendProviderKind) -> &'static str {
    match kind {
        BackendProviderKind::OpenAiCompatible => "OpenAI-compatible",
        BackendProviderKind::OpenRouter => "OpenRouter",
        BackendProviderKind::ChatGptCodex => "ChatGPT Codex",
        BackendProviderKind::XaiGrokOAuth => "Grok / xAI OAuth",
        BackendProviderKind::ClaudeCliSubscription => "Claude CLI subscription",
        BackendProviderKind::AnthropicApiKey => "Anthropic API key",
    }
}

const MODEL_DISCOVERY_PATH: &str = "/models";
const XAI_GROK_MODEL_DISCOVERY_PATH: &str = "/models-v2";
const CLAUDE_MODELS_BASE: &str =
    crate::inference::clients::claude_subscription::ANTHROPIC_API_ENDPOINT;

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
    name: Option<String>,
    #[serde(rename = "contextWindow")]
    context_window: Option<i64>,
    max_model_len: Option<i64>,
    context_length: Option<i64>,
    top_provider: Option<OpenRouterModelProvider>,
    display_name: Option<String>,
    max_input_tokens: Option<i64>,
    max_tokens: Option<i64>,
    capabilities: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct OpenRouterModelProvider {
    max_completion_tokens: Option<i64>,
}

impl OpenAiModelRecord {
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

    fn into_advertised(self, kind: BackendProviderKind, xai_api: bool) -> Option<AdvertisedModel> {
        let reasoning_efforts = if kind.uses_messages_wire() {
            self.capabilities.as_ref().map(|caps| {
                ["low", "medium", "high", "xhigh", "max"]
                    .into_iter()
                    .filter(|effort| caps["effort"][*effort]["supported"].as_bool() == Some(true))
                    .filter_map(|effort| {
                        crate::inference::resolved::ReasoningEffort::parse(effort).ok()
                    })
                    .collect()
            })
        } else if xai_api {
            Some(
                self.capabilities
                    .as_ref()
                    .and_then(|caps| caps["reasoning_effort"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .filter_map(|effort| {
                        crate::inference::resolved::ReasoningEffort::parse(effort).ok()
                    })
                    .collect(),
            )
        } else {
            None
        };
        let (name, context_window) = (
            if matches!(
                kind,
                BackendProviderKind::XaiGrokOAuth | BackendProviderKind::OpenRouter
            ) {
                self.name.clone()
            } else if kind.uses_messages_wire() {
                self.display_name.clone()
            } else {
                None
            },
            if kind == BackendProviderKind::XaiGrokOAuth {
                self.context_window
            } else if kind == BackendProviderKind::OpenRouter || xai_api {
                self.context_length
            } else if kind == BackendProviderKind::OpenAiCompatible {
                self.max_model_len
            } else if kind.uses_messages_wire() {
                self.max_input_tokens
            } else {
                None
            },
        );
        let max_output_tokens = if kind == BackendProviderKind::OpenRouter {
            self.top_provider
                .as_ref()
                .and_then(|provider| provider.max_completion_tokens)
                .filter(|value| *value > 0)
        } else if kind.uses_messages_wire() {
            self.max_tokens.filter(|value| *value > 0)
        } else {
            None
        };
        let model_name = self.identifier(kind)?;
        let display_name = name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && value != &model_name);
        Some(AdvertisedModel {
            model_name,
            display_name,
            context_window: context_window.filter(|value| *value > 0),
            max_context_window: None,
            max_output_tokens,
            reasoning_efforts,
        })
    }
}

#[derive(Deserialize)]
struct ChatGptCodexModelRecord {
    slug: Option<String>,
    id: Option<String>,
    name: Option<String>,
    model: Option<String>,
    display_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_i64_tolerant")]
    context_window: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_i64_tolerant")]
    max_context_window: Option<i64>,
    supported_reasoning_levels: Option<Vec<CodexReasoningLevel>>,
}

fn deserialize_optional_i64_tolerant<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<serde_json::Value>::deserialize(deserializer)?.and_then(|value| value.as_i64()))
}

#[derive(Deserialize)]
struct CodexReasoningLevel {
    effort: String,
}

impl ChatGptCodexModelRecord {
    fn identifier(self) -> Option<String> {
        [self.slug, self.id, self.name, self.model]
            .into_iter()
            .flatten()
            .find(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
    }

    fn into_advertised(self) -> Option<AdvertisedModel> {
        let display_name = self.display_name.clone();
        let context_window = self.context_window.filter(|v| *v > 0);
        let max_context_window = self
            .max_context_window
            .filter(|value| *value > 0)
            .filter(|value| context_window.is_some_and(|default| *value >= default));
        let reasoning_efforts = self.supported_reasoning_levels.as_ref().map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    crate::inference::resolved::ReasoningEffort::parse(&level.effort).ok()
                })
                .collect()
        });
        let model_name = self.identifier()?;
        Some(AdvertisedModel {
            model_name,
            display_name,
            context_window,
            max_context_window,
            max_output_tokens: None,
            reasoning_efforts,
        })
    }
}

pub async fn discover_models(
    client: &Client,
    kind: BackendProviderKind,
    endpoint: &str,
    api_key: Option<&str>,
    oauth_credential: Option<&crate::inference::auth::credential::OAuthCredential>,
) -> Result<Vec<AdvertisedModel>> {
    let xai_api = kind == BackendProviderKind::OpenAiCompatible
        && crate::inference::backends::setup::is_xai_api_endpoint(endpoint);
    let endpoint = match kind {
        BackendProviderKind::ChatGptCodex => {
            crate::inference::clients::chatgpt_codex::normalize_endpoint(endpoint)
        }
        BackendProviderKind::XaiGrokOAuth => {
            crate::inference::clients::xai_grok::normalize_endpoint(endpoint)
        }
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
                    "ChatGPT Codex model discovery requires an OAuthCredential document; run `gents codex-login` for the node DID first"
                );
            };
            request = request.bearer_auth(&credential.access_token);
            let headers = match crate::inference::clients::chatgpt_codex::build_chatgpt_codex_headers(
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
                crate::inference::clients::chatgpt_codex::chatgpt_codex_client_version(),
            )]);
        } else if kind == BackendProviderKind::XaiGrokOAuth {
            let Some(credential) = oauth_credential else {
                tracing::Span::current().record("failure_class", "auth");
                anyhow::bail!(
                    "Grok OAuth model discovery requires an OAuthCredential document; run `gents grok-login` for the node DID first"
                );
            };
            request = request.bearer_auth(&credential.access_token);
            let headers = match crate::inference::clients::xai_grok::build_xai_grok_oauth_headers() {
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
                    "Claude subscription model discovery requires an OAuthCredential document; run `gents claude-login --node-did <did>` for the node DID first"
                );
            };
            request = request
                .bearer_auth(&credential.access_token)
                .header(
                    "anthropic-version",
                    crate::inference::clients::claude_messages::ANTHROPIC_VERSION,
                )
                .header("anthropic-beta", crate::inference::clients::claude_messages::OAUTH_BETA)
                .query(&[("limit", "100")]);
        } else if kind == BackendProviderKind::AnthropicApiKey {
            if let Some(api_key) = api_key {
                request = request.bearer_auth(api_key);
            }
            request = request
                .header(
                    "anthropic-version",
                    crate::inference::clients::claude_messages::ANTHROPIC_VERSION,
                )
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
            .filter_map(|model| model.into_advertised(kind, xai_api));
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
mod tests;
