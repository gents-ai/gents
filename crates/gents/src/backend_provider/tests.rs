use super::*;

#[test]
fn openrouter_and_vllm_catalogs_preserve_advertised_limits() {
    let router: OpenAiModelRecord = serde_json::from_value(serde_json::json!({
        "id":"test", "name":"Test", "context_length":128000,
        "top_provider":{"max_completion_tokens":4096}
    }))
    .unwrap();
    let advertised = router
        .into_advertised(BackendProviderKind::OpenRouter, false)
        .unwrap();
    assert_eq!(advertised.context_window, Some(128000));
    assert_eq!(advertised.max_output_tokens, Some(4096));
    let local: OpenAiModelRecord = serde_json::from_value(serde_json::json!({
        "id":"GLM-5.3-Flash-NVFP4", "max_model_len":1048576
    }))
    .unwrap();
    assert_eq!(
        local
            .into_advertised(BackendProviderKind::OpenAiCompatible, false)
            .unwrap()
            .context_window,
        Some(1048576)
    );
}

#[test]
fn xai_api_catalog_records_effort_lists_and_context_length() {
    use crate::config::ReasoningEffort::{High, Low, Medium, None as Off, XHigh};
    let record = |value| serde_json::from_value::<OpenAiModelRecord>(value).unwrap();
    let grok_4_3 = || {
        record(serde_json::json!({
            "id":"grok-4.3", "object":"model", "context_length":1000000,
            "capabilities":{"reasoning_effort":["none","low","medium","high","xhigh"],
                "default_reasoning_effort":"low"}
        }))
    };
    let grok_4_5 = record(serde_json::json!({
        "id":"grok-4.5", "object":"model", "context_length":2000000,
        "capabilities":{"reasoning_effort":["low","medium","high","xhigh"],
            "default_reasoning_effort":"high"}
    }));
    let grok_4_20 = record(serde_json::json!({
        "id":"grok-4.20-0309-reasoning", "object":"model", "context_length":2000000
    }));
    let xai = |record: OpenAiModelRecord| {
        record
            .into_advertised(BackendProviderKind::OpenAiCompatible, true)
            .unwrap()
    };
    let advertised = xai(grok_4_3());
    assert_eq!(
        advertised.reasoning_efforts,
        Some(vec![Off, Low, Medium, High, XHigh])
    );
    assert_eq!(advertised.context_window, Some(1000000));
    let advertised = xai(grok_4_5);
    assert_eq!(
        advertised.reasoning_efforts,
        Some(vec![Low, Medium, High, XHigh])
    );
    assert_eq!(advertised.context_window, Some(2000000));
    let advertised = xai(grok_4_20);
    assert_eq!(advertised.reasoning_efforts, Some(vec![]));
    assert_eq!(advertised.context_window, Some(2000000));

    let other = grok_4_3()
        .into_advertised(BackendProviderKind::OpenAiCompatible, false)
        .unwrap();
    assert_eq!(other.reasoning_efforts, None);
    assert_eq!(other.context_window, None);
}

#[test]
fn codex_catalog_preserves_context_and_supported_efforts() {
    let record: ChatGptCodexModelRecord = serde_json::from_value(serde_json::json!({
        "slug": "gpt-5.6-sol", "display_name": "GPT-5.6 Sol",
        "context_window": 272000, "max_context_window": 872000,
        "supported_reasoning_levels": [{"effort":"low"}, {"effort":"ultra"}, {"effort":"future"}]
    }))
    .unwrap();
    let advertised = record.into_advertised().unwrap();
    assert_eq!(advertised.context_window, Some(272000));
    assert_eq!(advertised.max_context_window, Some(872000));
    assert_eq!(advertised.display_name.as_deref(), Some("GPT-5.6 Sol"));
    assert_eq!(
        advertised.reasoning_efforts,
        Some(vec![
            crate::config::ReasoningEffort::Low,
            crate::config::ReasoningEffort::Ultra
        ])
    );
    assert_eq!(advertised.max_output_tokens, None);
}

#[test]
fn codex_catalog_distinguishes_unknown_and_disabled_reasoning() {
    for (fields, expected) in [
        (serde_json::json!({}), None),
        (
            serde_json::json!({"supported_reasoning_levels": []}),
            Some(vec![]),
        ),
    ] {
        let mut value = serde_json::json!({"slug":"test", "max_context_window":128000});
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let record: ChatGptCodexModelRecord = serde_json::from_value(value).unwrap();
        let advertised = record.into_advertised().unwrap();
        assert_eq!(advertised.context_window, None);
        assert_eq!(advertised.max_context_window, None);
        assert_eq!(advertised.reasoning_efforts, expected);
    }
}

#[test]
fn codex_catalog_discards_invalid_or_inverted_context_ceilings() {
    for (default, maximum) in [(272_000, 0), (272_000, -1), (272_000, 128_000)] {
        let record: ChatGptCodexModelRecord = serde_json::from_value(serde_json::json!({
            "slug": "gpt-test",
            "context_window": default,
            "max_context_window": maximum
        }))
        .unwrap();
        let advertised = record.into_advertised().unwrap();
        assert_eq!(advertised.context_window, Some(default));
        assert_eq!(advertised.max_context_window, None);
    }
}

#[test]
fn codex_catalog_retains_model_when_context_metadata_is_malformed() {
    for malformed in [
        serde_json::json!("872000"),
        serde_json::json!({"tokens": 872000}),
        serde_json::json!(872000.5),
    ] {
        let record: ChatGptCodexModelRecord = serde_json::from_value(serde_json::json!({
            "slug": "gpt-test",
            "context_window": malformed,
            "max_context_window": malformed
        }))
        .unwrap();
        let advertised = record.into_advertised().unwrap();
        assert_eq!(advertised.model_name, "gpt-test");
        assert_eq!(advertised.context_window, None);
        assert_eq!(advertised.max_context_window, None);
    }
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
    let (endpoint, requests) = spawn_model_discovery_server(
        r#"{"data":[{"id":"row-1","model":"grok-4.5","name":"Grok 4.5","contextWindow":256000,"apiBackend":"responses"},{"id":"row-2","modelId":"grok-build-0.1","name":"Grok Build"}]}"#,
    )
    .await;
    let credential = grok_credential();

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
async fn discover_models_grok_426_is_a_client_version_gate() {
    let (endpoint, _requests) = spawn_model_discovery_server_with_status(
        "426 Upgrade Required",
        r#"{"error":{"message":"Your Grok CLI version (1.0.13) is outdated. Please update to version 1.0.46 or later"}}"#,
    )
    .await;
    let credential = grok_credential();

    let error = discover_models(
        &Client::new(),
        BackendProviderKind::XaiGrokOAuth,
        &endpoint,
        None,
        Some(&credential),
    )
    .await
    .expect_err("426 from /models-v2 must fail discovery");

    let http = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ModelDiscoveryHttpError>())
        .expect(
            "426 must surface as ModelDiscoveryHttpError so the CLI appends version-gate guidance",
        );
    assert_eq!(http.status, 426, "{http}");
    assert!(
        http.is_client_version_gate(),
        "426 is the client-version gate, not an auth failure: {http}"
    );
    assert!(!http.is_auth(), "{http}");
}

#[tokio::test]
async fn discover_models_sends_chatgpt_codex_version_header_and_query_param() {
    let (endpoint, requests) =
        spawn_model_discovery_server(
            r#"{"models":[{"slug":"gpt-6.1-sol","display_name":"GPT-6.1 Sol","context_window":272000,"max_context_window":872000,"supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"},{"effort":"xhigh"},{"effort":"max"},{"effort":"ultra"}]}]}"#,
        ).await;
    let credential = crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: "chatgpt-codex:did:key:zAgent".to_string(),
        node_did: "did:key:zAgent".to_string(),
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
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
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

    assert_eq!(model_names(&models), vec!["gpt-6.1-sol"]);
    assert_eq!(models[0].context_window, Some(272000));
    assert_eq!(models[0].max_context_window, Some(872000));
    assert_eq!(
        models[0].reasoning_efforts.as_deref(),
        Some(
            &[
                crate::config::ReasoningEffort::Low,
                crate::config::ReasoningEffort::Medium,
                crate::config::ReasoningEffort::High,
                crate::config::ReasoningEffort::XHigh,
                crate::config::ReasoningEffort::Max,
                crate::config::ReasoningEffort::Ultra,
            ][..]
        )
    );
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

fn claude_credential() -> crate::inference::auth::credential::OAuthCredential {
    crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: "claude-subscription:did:key:zAgent".to_string(),
        node_did: "did:key:zAgent".to_string(),
        provider: crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER.to_string(),
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    }
}

fn grok_credential() -> crate::inference::auth::credential::OAuthCredential {
    crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: "xai-oauth:did:key:zAgent".to_string(),
        node_did: "did:key:zAgent".to_string(),
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
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    }
}

#[tokio::test]
async fn discover_models_reads_claude_models_with_oauth_headers() {
    let (endpoint, requests) = spawn_model_discovery_server(
        r#"{"data":[{"id":"claude-fable-5-1","display_name":"Claude Fable 5.1","max_input_tokens":1000000,"max_tokens":128000,"capabilities":{"effort":{"supported":true,"low":{"supported":true},"high":{"supported":true},"xhigh":{"supported":true}}},"type":"model"},{"id":"claude-opus-5","type":"model"},{"id":"claude-sonnet-5","type":"model"}],"has_more":false}"#,
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
    assert_eq!(models[0].context_window, Some(1_000_000));
    assert_eq!(models[0].max_output_tokens, Some(128_000));
    assert_eq!(models[0].display_name.as_deref(), Some("Claude Fable 5.1"));
    assert_eq!(
        models[0].reasoning_efforts.as_deref(),
        Some(
            [
                crate::config::ReasoningEffort::Low,
                crate::config::ReasoningEffort::High,
                crate::config::ReasoningEffort::XHigh,
            ]
            .as_slice()
        )
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
async fn discover_models_reads_anthropic_key_models_without_oauth_beta() {
    let (endpoint, requests) = spawn_model_discovery_server(
        r#"{"data":[{"id":"claude-fable-5-1","display_name":"Claude Fable 5.1","max_input_tokens":1000000,"max_tokens":128000,"capabilities":{"effort":{"supported":true,"low":{"supported":true},"high":{"supported":true},"xhigh":{"supported":true}}},"type":"model"},{"id":"claude-opus-5","type":"model"},{"id":"claude-sonnet-5","type":"model"}],"has_more":false}"#,
    )
    .await;

    let models = discover_models(
        &Client::new(),
        BackendProviderKind::AnthropicApiKey,
        &format!("{endpoint}/v1"),
        Some("placeholder-key"),
        None,
    )
    .await
    .expect("Anthropic key model discovery should read /v1/models");

    assert_eq!(
        model_names(&models),
        vec!["claude-fable-5-1", "claude-opus-5", "claude-sonnet-5"]
    );
    assert_eq!(models[0].context_window, Some(1_000_000));
    assert_eq!(models[0].max_output_tokens, Some(128_000));
    assert_eq!(models[0].display_name.as_deref(), Some("Claude Fable 5.1"));
    assert_eq!(
        models[0].reasoning_efforts.as_deref(),
        Some(
            [
                crate::config::ReasoningEffort::Low,
                crate::config::ReasoningEffort::High,
                crate::config::ReasoningEffort::XHigh,
            ]
            .as_slice()
        )
    );
    let requests = requests.lock().expect("requests lock");
    let request = requests
        .first()
        .expect("captured request")
        .to_ascii_lowercase();
    assert!(
        request.starts_with("get /v1/models?limit=100 "),
        "key discovery must query /v1/models?limit=100: {request}"
    );
    for header in [
        "authorization: bearer placeholder-key",
        "anthropic-version: 2023-06-01",
    ] {
        assert!(request.contains(header), "{header} missing from {request}");
    }
    assert!(!request.contains("anthropic-beta"), "{request}");
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
        .expect(
            "401 must surface as ModelDiscoveryHttpError so the CLI appends claude-login guidance",
        );
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
        message.contains("requires an OAuthCredential document") && message.contains("--node-did"),
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
