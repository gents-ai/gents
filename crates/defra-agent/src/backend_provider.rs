use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::Instrument;

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
    chatgpt_credential: Option<&crate::chatgpt_codex::OAuthCredential>,
) -> Result<Vec<String>> {
    let endpoint = if kind == BackendProviderKind::ChatGptCodex {
        crate::chatgpt_codex::normalize_endpoint(endpoint)
    } else {
        endpoint.trim_end_matches('/').to_string()
    };
    let models_url = format!("{}{}", endpoint, MODEL_DISCOVERY_PATH);
    let provider_name = provider_display_name(kind);
    async {
        let mut request = client.get(&models_url);
        if kind == BackendProviderKind::ChatGptCodex {
            let Some(credential) = chatgpt_credential else {
                tracing::Span::current().record("failure_class", "auth");
                anyhow::bail!(
                    "ChatGPT Codex model discovery requires an OAuthCredential document; run `defra-agent codex-login` for the agent DID first"
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
            // Must match the request `version` header: /models gates the returned model set on
            // the advertised Codex client version (defra-agent's own version returns an empty set).
            request = request.query(&[(
                "client_version",
                crate::chatgpt_codex::chatgpt_codex_client_version(),
            )]);
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
            anyhow::bail!(
                "{} model discovery failed at {}: {} {}",
                provider_name,
                models_url,
                status,
                truncate_probe_body(&body)
            );
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

        let openai_models = models.data.into_iter().map(|model| model.id);
        let chatgpt_codex_models = models.models.into_iter().map(|model| model.slug);
        let models = openai_models
            .chain(chatgpt_codex_models)
            .collect::<Vec<_>>();
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
    format!("{}...", &body[..LIMIT])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn discover_models_reads_openai_models_and_sends_api_key() {
        let (endpoint, requests) =
            spawn_model_discovery_server(r#"{"data":[{"id":"gpt-4.1-mini"},{"id":"o3"}]}"#).await;

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::OpenAiCompatible,
            &format!("{endpoint}/v1/"),
            Some("sk-test"),
            None,
        )
        .await
        .expect("model discovery should succeed");

        assert_eq!(models, vec!["gpt-4.1-mini", "o3"]);
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
            spawn_model_discovery_server(r#"{"models":[{"slug":"codex-mini-latest"}]}"#).await;

        let models = discover_models(
            &Client::new(),
            BackendProviderKind::OpenAiCompatible,
            &endpoint,
            None,
            None,
        )
        .await
        .expect("model discovery should accept the Codex-compatible models shape");

        assert_eq!(models, vec!["codex-mini-latest"]);
    }

    async fn spawn_model_discovery_server(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
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
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
