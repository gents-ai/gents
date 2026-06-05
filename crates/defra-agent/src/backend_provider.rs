use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BackendProviderKind {
    #[default]
    #[serde(
        rename = "OpenAiCompatible",
        alias = "openai-compatible",
        alias = "openai_compatible",
        alias = "openai",
        alias = "OpenAICompatible"
    )]
    OpenAiCompatible,
    #[serde(rename = "OpenRouter", alias = "openrouter")]
    OpenRouter,
    #[serde(
        rename = "ChatGptCodex",
        alias = "ChatGPTCodex",
        alias = "chatgpt-codex",
        alias = "chatgpt_codex",
        alias = "codex-chatgpt",
        alias = "codex"
    )]
    ChatGptCodex,
}

impl BackendProviderKind {
    pub fn parse_optional(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(Self::default()),
            Some("OpenAiCompatible")
            | Some("OpenAICompatible")
            | Some("openai-compatible")
            | Some("openai_compatible")
            | Some("openai") => Ok(Self::OpenAiCompatible),
            Some("OpenRouter") | Some("openrouter") => Ok(Self::OpenRouter),
            Some("ChatGptCodex")
            | Some("ChatGPTCodex")
            | Some("chatgpt-codex")
            | Some("chatgpt_codex")
            | Some("codex-chatgpt")
            | Some("codex") => Ok(Self::ChatGptCodex),
            Some(other) => anyhow::bail!("unknown backend provider kind {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAiCompatible",
            Self::OpenRouter => "OpenRouter",
            Self::ChatGptCodex => "ChatGptCodex",
        }
    }
}

impl std::fmt::Display for BackendProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn provider_display_name(kind: BackendProviderKind) -> &'static str {
    match kind {
        BackendProviderKind::OpenAiCompatible => "OpenAI-compatible",
        BackendProviderKind::OpenRouter => "OpenRouter",
        BackendProviderKind::ChatGptCodex => "ChatGPT Codex",
    }
}

const MODEL_DISCOVERY_PATH: &str = "/models";

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModelRecord>,
    #[serde(default)]
    models: Vec<ChatGptCodexModelRecord>,
}

#[derive(Deserialize)]
struct OpenAiModelRecord {
    id: String,
}

#[derive(Deserialize)]
struct ChatGptCodexModelRecord {
    slug: String,
}

pub async fn discover_models(
    client: &Client,
    kind: BackendProviderKind,
    endpoint: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>> {
    let endpoint = if kind == BackendProviderKind::ChatGptCodex {
        crate::chatgpt_codex::normalize_endpoint(endpoint)
    } else {
        endpoint.trim_end_matches('/').to_string()
    };
    let models_url = format!("{}{}", endpoint, MODEL_DISCOVERY_PATH);
    let provider_name = provider_display_name(kind);
    let mut request = client.get(&models_url);
    if kind == BackendProviderKind::ChatGptCodex {
        let (_codex_home, auth) = crate::chatgpt_codex::load_default_chatgpt_auth().await?;
        let access_token = auth
            .get_token()
            .context("ChatGPT Codex auth did not expose a bearer token")?;
        request = request.bearer_auth(access_token);
        for (name, value) in crate::chatgpt_codex::build_chatgpt_codex_headers(&auth)? {
            if let Some(name) = name {
                request = request.header(name, value);
            }
        }
        request = request.query(&[("client_version", env!("CARGO_PKG_VERSION"))]);
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
    let response = request
        .send()
        .await
        .with_context(|| format!("querying {} models endpoint {}", provider_name, models_url))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable body>".to_string());
    if !status.is_success() {
        anyhow::bail!(
            "{} model discovery failed at {}: {} {}",
            provider_name,
            models_url,
            status,
            truncate_probe_body(&body)
        );
    }

    let models: OpenAiModelsResponse = serde_json::from_str(&body).with_context(|| {
        format!(
            "decoding {} model discovery response from {}: {}",
            provider_name,
            models_url,
            truncate_probe_body(&body)
        )
    })?;

    let openai_models = models.data.into_iter().map(|model| model.id);
    let chatgpt_codex_models = models.models.into_iter().map(|model| model.slug);
    Ok(openai_models.chain(chatgpt_codex_models).collect())
}

pub fn truncate_probe_body(body: &str) -> String {
    const LIMIT: usize = 256;
    if body.len() <= LIMIT {
        return body.to_string();
    }
    format!("{}...", &body[..LIMIT])
}
