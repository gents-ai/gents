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
            Some(other) => anyhow::bail!("unknown backend provider kind {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAiCompatible",
            Self::OpenRouter => "OpenRouter",
        }
    }
}

impl std::fmt::Display for BackendProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub trait BackendProviderAdapter: Send + Sync {
    fn kind(&self) -> BackendProviderKind;

    fn display_name(&self) -> &'static str;

    fn verification_path(&self) -> &'static str;

    fn model_discovery_path(&self) -> &'static str {
        "/models"
    }

    fn build_completion_client(
        &self,
        endpoint: &str,
        api_key: &str,
    ) -> Result<rig::providers::openai::CompletionsClient> {
        rig::providers::openai::CompletionsClient::builder()
            .api_key(api_key)
            .base_url(endpoint)
            .build()
            .with_context(|| {
                format!(
                    "building {} completion client for backend endpoint {}",
                    self.display_name(),
                    endpoint
                )
            })
    }
}

#[derive(Debug, Default)]
struct OpenAiCompatibleAdapter;

impl BackendProviderAdapter for OpenAiCompatibleAdapter {
    fn kind(&self) -> BackendProviderKind {
        BackendProviderKind::OpenAiCompatible
    }

    fn display_name(&self) -> &'static str {
        "OpenAI-compatible"
    }

    fn verification_path(&self) -> &'static str {
        "/models"
    }
}

#[derive(Debug, Default)]
struct OpenRouterAdapter;

impl BackendProviderAdapter for OpenRouterAdapter {
    fn kind(&self) -> BackendProviderKind {
        BackendProviderKind::OpenRouter
    }

    fn display_name(&self) -> &'static str {
        "OpenRouter"
    }

    fn verification_path(&self) -> &'static str {
        "/key"
    }
}

static OPENAI_COMPATIBLE_ADAPTER: OpenAiCompatibleAdapter = OpenAiCompatibleAdapter;
static OPENROUTER_ADAPTER: OpenRouterAdapter = OpenRouterAdapter;

pub fn adapter_for(kind: BackendProviderKind) -> &'static dyn BackendProviderAdapter {
    match kind {
        BackendProviderKind::OpenAiCompatible => &OPENAI_COMPATIBLE_ADAPTER,
        BackendProviderKind::OpenRouter => &OPENROUTER_ADAPTER,
    }
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModelRecord>,
}

#[derive(Deserialize)]
struct OpenAiModelRecord {
    id: String,
}

pub fn build_completion_client(
    kind: BackendProviderKind,
    endpoint: &str,
    api_key: &str,
) -> Result<rig::providers::openai::CompletionsClient> {
    adapter_for(kind).build_completion_client(endpoint, api_key)
}

pub async fn discover_models(
    client: &Client,
    kind: BackendProviderKind,
    endpoint: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>> {
    let adapter = adapter_for(kind);
    let models_url = format!(
        "{}{}",
        endpoint.trim_end_matches('/'),
        adapter.model_discovery_path()
    );
    let mut request = client.get(&models_url);
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().await.with_context(|| {
        format!(
            "querying {} models endpoint {}",
            adapter.display_name(),
            models_url
        )
    })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable body>".to_string());
    if !status.is_success() {
        anyhow::bail!(
            "{} model discovery failed at {}: {} {}",
            adapter.display_name(),
            models_url,
            status,
            truncate_probe_body(&body)
        );
    }

    let models: OpenAiModelsResponse = serde_json::from_str(&body).with_context(|| {
        format!(
            "decoding {} model discovery response from {}: {}",
            adapter.display_name(),
            models_url,
            truncate_probe_body(&body)
        )
    })?;

    Ok(models.data.into_iter().map(|model| model.id).collect())
}

pub fn truncate_probe_body(body: &str) -> String {
    const LIMIT: usize = 256;
    if body.len() <= LIMIT {
        return body.to_string();
    }
    format!("{}...", &body[..LIMIT])
}
