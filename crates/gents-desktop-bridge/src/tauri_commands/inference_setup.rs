//! First-party inference onboarding commands for the desktop app.
//!
//! Rust owns the versioned provider catalog, canonical connection mapping,
//! advertised-model discovery, and Gents recommendations. OAuth credentials
//! remain agent-scoped documents and never cross this bridge unredacted. The UI
//! commits the chosen canonical components through the existing atomic operator
//! configuration command.

use std::time::Duration;

use gents::chatgpt_codex::normalize_provider;
use gents::inference_setup::{
    InferenceAuthMethod, InferenceModelOption, InferenceModelRecommendation, InferenceProviderId,
    InferenceSetupCatalog,
};
use gents::oauth_credential::{list_oauth_credentials_on, OAuthCredential};
use gents_chatgpt_login::{run_login_server, LoginOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::error::BridgeError;
use crate::state::{current_core, DesktopAppState};
use crate::types::ClientUpdateEvent;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

const CODEX_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceDiscoveryFailure {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct InferenceDiscoveryRequest {
    pub request_key: String,
    pub agent_did: String,
    pub provider: InferenceProviderId,
    pub auth_method: InferenceAuthMethod,
    pub endpoint: String,
    /// Ephemeral connection input. It is consumed for this request and is
    /// never reflected into a response or desktop snapshot.
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceDiscoveryResult {
    pub request_key: String,
    pub contract_version: u32,
    pub defaults_version: String,
    pub requested_endpoint: String,
    pub effective_endpoint: String,
    pub backend_name: String,
    pub provider_kind: gents::BackendProviderKind,
    pub openai_wire_api: Option<gents::OpenAiWireApi>,
    pub reachable: bool,
    pub models: Vec<InferenceModelOption>,
    pub failure: Option<InferenceDiscoveryFailure>,
    pub manual_entry_allowed: bool,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct InferenceRecommendationRequest {
    pub provider: InferenceProviderId,
    pub auth_method: InferenceAuthMethod,
    pub model_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub reasoning_efforts: Option<Vec<gents::config::ReasoningEffort>>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct InferenceBackendRecommendationRequest {
    pub provider_kind: gents::BackendProviderKind,
    pub endpoint: String,
    pub model_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub reasoning_efforts: Option<Vec<gents::config::ReasoningEffort>>,
}

#[tauri::command]
pub(crate) fn desktop_inference_setup_catalog() -> InferenceSetupCatalog {
    gents::inference_setup::inference_setup_catalog()
}

#[tauri::command]
pub(crate) fn desktop_inference_model_recommendation(
    request: InferenceRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    let model_name = request.model_name.trim();
    if model_name.is_empty() {
        return Err(BridgeError::untyped("model name is required"));
    }
    gents::inference_setup::recommendation_for_model(
        request.provider,
        request.auth_method,
        &gents::document_config::AdvertisedModel {
            model_name: model_name.to_string(),
            display_name: request.display_name,
            context_window: request.context_window,
            max_output_tokens: request.max_output_tokens,
            reasoning_efforts: request.reasoning_efforts,
        },
    )
    .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub(crate) fn desktop_inference_backend_recommendation(
    request: InferenceBackendRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    let (provider, auth_method) = gents::inference_setup::provider_selection_for_backend(
        request.provider_kind,
        &request.endpoint,
    );
    desktop_inference_model_recommendation(InferenceRecommendationRequest {
        provider,
        auth_method,
        model_name: request.model_name,
        display_name: request.display_name,
        context_window: request.context_window,
        max_output_tokens: request.max_output_tokens,
        reasoning_efforts: request.reasoning_efforts,
    })
}

fn classify_discovery_failure(error: &anyhow::Error) -> (&'static str, bool, bool) {
    let http_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<gents::backend_provider::ModelDiscoveryHttpError>());
    let is_decode = error
        .chain()
        .any(|cause| cause.downcast_ref::<serde_json::Error>().is_some());
    if let Some(error) = http_error {
        if error.is_auth() {
            ("authentication", true, false)
        } else {
            // A non-auth HTTP response proves the configured host is
            // reachable even when it does not expose a model catalog.
            ("http", true, true)
        }
    } else if is_decode {
        ("decode", true, true)
    } else {
        ("connection", false, false)
    }
}

#[tauri::command]
pub(crate) async fn desktop_inference_models_discover(
    request: InferenceDiscoveryRequest,
    state: State<'_, DesktopAppState>,
) -> Result<InferenceDiscoveryResult, BridgeError> {
    let spec = gents::inference_setup::connection_spec(
        request.provider,
        request.auth_method,
        &request.endpoint,
    )
    .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let api_key = request
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if spec.api_key_required && api_key.is_none() {
        return Err(BridgeError::untyped("API key is required"));
    }

    let credential = if let Some(provider) = spec.oauth_provider {
        let core = current_core(&state)
            .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
        let access = core
            .operator_access(request.agent_did.trim())
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        gents::oauth_credential::list_oauth_credentials_on(&access, request.agent_did.trim())
            .await
            .map_err(|error| BridgeError::untyped(error.to_string()))?
            .into_iter()
            .find(|credential| credential.enabled && credential.provider == provider)
    } else {
        None
    };
    if spec.oauth_provider.is_some() && credential.is_none() {
        return Err(BridgeError::untyped(
            "the selected provider account is not connected",
        ));
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let requested_endpoint = request.endpoint.trim().to_string();
    let discovered = gents::discover_backend_models(
        &client,
        spec.provider_kind,
        &spec.endpoint,
        api_key,
        credential.as_ref(),
    )
    .await;

    let (reachable, models, failure, manual_entry_allowed) = match discovered {
        Ok(models) if models.is_empty() => (
            true,
            Vec::new(),
            Some(InferenceDiscoveryFailure {
                kind: "empty_catalog".into(),
                message: "The connection succeeded, but the provider advertised no models.".into(),
            }),
            true,
        ),
        Ok(models) => {
            let models = models
                .into_iter()
                .map(|model| {
                    gents::inference_setup::model_option(
                        request.provider,
                        request.auth_method,
                        model,
                    )
                })
                .collect::<anyhow::Result<Vec<_>>>()
                .map_err(|error| BridgeError::untyped(error.to_string()))?;
            (true, models, None, false)
        }
        Err(error) => {
            let (kind, reachable, manual_entry_allowed) = classify_discovery_failure(&error);
            (
                reachable,
                Vec::new(),
                Some(InferenceDiscoveryFailure {
                    kind: kind.into(),
                    message: format!("{error:#}"),
                }),
                manual_entry_allowed,
            )
        }
    };

    Ok(InferenceDiscoveryResult {
        request_key: request.request_key,
        contract_version: gents::inference_setup::INFERENCE_SETUP_CONTRACT_VERSION,
        defaults_version: gents::inference_setup::INFERENCE_DEFAULTS_VERSION.into(),
        requested_endpoint,
        effective_endpoint: spec.endpoint,
        backend_name: spec.backend_name.into(),
        provider_kind: spec.provider_kind,
        openai_wire_api: spec.openai_wire_api,
        reachable,
        models,
        manual_entry_allowed,
        failure,
    })
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceProbeRequest {
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceProbeResult {
    pub reachable: bool,
    pub models: Vec<String>,
}

#[tauri::command]
pub(crate) async fn desktop_probe_inference_endpoint(
    request: InferenceProbeRequest,
) -> Result<InferenceProbeResult, BridgeError> {
    Ok(probe_inference_models(&request.endpoint).await)
}

async fn probe_inference_models(base: &str) -> InferenceProbeResult {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return InferenceProbeResult {
            reachable: false,
            models: Vec::new(),
        };
    }
    let models = async {
        let response = reqwest::Client::new()
            .get(format!("{base}/models"))
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: Value = response.json().await.ok()?;
        let models = body
            .get("data")?
            .as_array()?
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>();
        Some(models)
    }
    .await;

    match models {
        Some(models) => InferenceProbeResult {
            reachable: true,
            models,
        },
        None => InferenceProbeResult {
            reachable: false,
            models: Vec::new(),
        },
    }
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexLoginRequest {
    pub agent_did: String,
    #[serde(default)]
    pub provider: Option<String>,
}

/// A redacted view of a stored credential. Tokens never cross the bridge into
/// the webview — only the metadata the UI needs to confirm the login worked.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexLoginResult {
    pub doc_id: String,
    pub credential_id: String,
    pub agent_did: String,
    pub provider: String,
    pub account_id: Option<String>,
    pub chatgpt_plan_type: Option<String>,
    pub is_fedramp: bool,
    pub access_token_expires_at: String,
    pub enabled: bool,
}

impl CodexLoginResult {
    fn redacted(doc_id: String, credential: &OAuthCredential) -> Self {
        Self {
            doc_id,
            credential_id: credential.credential_id.clone(),
            agent_did: credential.agent_did.clone(),
            provider: credential.provider.clone(),
            account_id: credential.account_id.clone(),
            chatgpt_plan_type: credential.chatgpt_plan_type.clone(),
            is_fedramp: credential.is_fedramp,
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            enabled: credential.enabled,
        }
    }
}

#[tauri::command]
pub(crate) async fn desktop_codex_login<R: Runtime>(
    app: AppHandle<R>,
    request: CodexLoginRequest,
    state: State<'_, DesktopAppState>,
) -> Result<CodexLoginResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        return Err(BridgeError::untyped("agent_did is required"));
    }
    let provider = normalize_provider(request.provider.as_deref().unwrap_or_default());

    let server = run_login_server(LoginOptions::default())
        .map_err(|error| BridgeError::untyped(format!("starting ChatGPT login server: {error}")))?;
    let _ = app.emit(
        "desktop://codex-login-url",
        CodexLoginUrl {
            url: server.auth_url.clone(),
        },
    );

    let cancel = server.cancel_handle();
    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.codex_login_cancel = Some(cancel.clone());
    }
    let wait = tokio::time::timeout(CODEX_LOGIN_TIMEOUT, server.block_until_done()).await;
    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.codex_login_cancel = None;
    }
    let tokens = match wait {
        Ok(result) => result.map_err(|error| {
            BridgeError::untyped(format!("ChatGPT browser login failed: {error}"))
        })?,
        Err(_elapsed) => {
            cancel.shutdown();
            return Err(BridgeError::untyped(
                "ChatGPT sign-in timed out waiting for the browser",
            ));
        }
    };

    let credential = OAuthCredential::from_login_tokens(
        &agent_did,
        &provider,
        &tokens.id_token,
        tokens.access_token,
        tokens.refresh_token,
        chrono::Utc::now(),
    );
    let access = core
        .operator_access(&agent_did)
        .map_err(|error| BridgeError::untyped(format!("storing ChatGPT credential: {error}")))?;
    let doc_id = gents::oauth_credential::upsert_oauth_credential_on(&access, &credential)
        .await
        .map_err(|error| BridgeError::untyped(format!("storing ChatGPT credential: {error}")))?;

    // Storing the credential is exactly the signal the runtime reconciles on to
    // flip a ChatGptCodex behavior available; nudge the UI to refetch health.
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );

    Ok(CodexLoginResult::redacted(doc_id, &credential))
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexLoginUrl {
    pub url: String,
}

#[tauri::command]
pub(crate) fn desktop_codex_login_cancel(
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let handle = {
        let mut bridge = state
            .bridge
            .lock()
            .map_err(|_| BridgeError::untyped("desktop bridge lock poisoned"))?;
        bridge.codex_login_cancel.take()
    };
    if let Some(handle) = handle {
        handle.shutdown();
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrokLoginRequest {
    pub agent_did: String,
    #[serde(default)]
    pub provider: Option<String>,
}

/// Redacted credential metadata for the webview (tokens never cross the bridge).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrokLoginResult {
    pub doc_id: String,
    pub credential_id: String,
    pub agent_did: String,
    pub provider: String,
    pub access_token_expires_at: String,
    pub enabled: bool,
}

impl GrokLoginResult {
    fn redacted(doc_id: String, credential: &OAuthCredential) -> Self {
        Self {
            doc_id,
            credential_id: credential.credential_id.clone(),
            agent_did: credential.agent_did.clone(),
            provider: credential.provider.clone(),
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            enabled: credential.enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrokLoginUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountView {
    pub credential_id: String,
    pub agent_did: String,
    pub provider: String,
    pub account_id: Option<String>,
    pub plan_type: Option<String>,
    pub access_token_expires_at: String,
    pub last_refresh: Option<String>,
    pub enabled: bool,
}

impl From<&OAuthCredential> for ProviderAccountView {
    fn from(credential: &OAuthCredential) -> Self {
        Self {
            credential_id: credential.credential_id.clone(),
            agent_did: credential.agent_did.clone(),
            provider: credential.provider.clone(),
            account_id: gents::oauth_credential::account_display_label(credential),
            plan_type: credential.chatgpt_plan_type.clone(),
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            last_refresh: credential.last_refresh.map(|value| value.to_rfc3339()),
            enabled: credential.enabled,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountsRequest {
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountDisconnectRequest {
    pub agent_did: String,
    pub credential_id: String,
}

#[tauri::command]
pub(crate) async fn desktop_provider_accounts_list(
    request: ProviderAccountsRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<ProviderAccountView>, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let agent_did = request.agent_did.trim();
    let access = core
        .operator_access(agent_did)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let credentials = list_oauth_credentials_on(&access, agent_did)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    Ok(credentials.iter().map(ProviderAccountView::from).collect())
}

#[tauri::command]
pub(crate) async fn desktop_provider_account_disconnect<R: Runtime>(
    app: AppHandle<R>,
    request: ProviderAccountDisconnectRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let access = core
        .operator_access(request.agent_did.trim())
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let credentials = list_oauth_credentials_on(&access, request.agent_did.trim())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let mut credential = credentials
        .into_iter()
        .find(|entry| entry.credential_id == request.credential_id)
        .ok_or_else(|| BridgeError::untyped("provider account not found"))?;
    credential.enabled = false;
    gents::oauth_credential::upsert_oauth_credential_on(&access, &credential)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
    Ok(())
}

#[tauri::command]
pub(crate) async fn desktop_grok_login<R: Runtime>(
    app: AppHandle<R>,
    request: GrokLoginRequest,
    state: State<'_, DesktopAppState>,
) -> Result<GrokLoginResult, BridgeError> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use gents::xai_grok_oauth::normalize_provider as normalize_xai_provider;
    use gents::xai_oauth_login::{
        credential_from_login_tokens, run_device_code_login_with_url_callback,
    };

    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        return Err(BridgeError::untyped("agent_did is required"));
    }
    let provider = normalize_xai_provider(request.provider.as_deref().unwrap_or_default());

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.grok_login_cancel = Some(cancel.clone());
    }

    let http = reqwest::Client::new();
    let app_for_url = app.clone();
    let login = tokio::time::timeout(
        CODEX_LOGIN_TIMEOUT,
        run_device_code_login_with_url_callback(&http, Some(cancel.clone()), move |url| {
            let _ = app_for_url.emit(
                crate::contract::GROK_LOGIN_URL_EVENT,
                GrokLoginUrl {
                    url: url.to_string(),
                },
            );
            if let Err(error) = webbrowser::open(url) {
                tracing::warn!(%error, "could not open the Grok login URL in a browser");
            }
        }),
    )
    .await;

    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.grok_login_cancel = None;
    }

    let tokens = match login {
        Ok(Ok(tokens)) => tokens,
        Ok(Err(error)) => {
            return Err(BridgeError::untyped(format!(
                "Grok device-code login failed: {error}"
            )));
        }
        Err(_elapsed) => {
            cancel.store(true, Ordering::SeqCst);
            return Err(BridgeError::untyped(
                "Grok sign-in timed out waiting for browser approval",
            ));
        }
    };

    let credential =
        credential_from_login_tokens(&agent_did, &provider, &tokens, chrono::Utc::now());
    let access = core
        .operator_access(&agent_did)
        .map_err(|error| BridgeError::untyped(format!("storing Grok credential: {error}")))?;
    let doc_id = gents::oauth_credential::upsert_oauth_credential_on(&access, &credential)
        .await
        .map_err(|error| BridgeError::untyped(format!("storing Grok credential: {error}")))?;

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );

    Ok(GrokLoginResult::redacted(doc_id, &credential))
}

#[cfg(test)]
mod provider_account_tests {
    use super::*;

    #[test]
    fn manual_model_entry_requires_a_reachable_discovery_endpoint() {
        let auth = anyhow::Error::new(gents::backend_provider::ModelDiscoveryHttpError {
            provider: "test".into(),
            url: "http://test/models".into(),
            status: 401,
            body: "unauthorized".into(),
        });
        assert_eq!(
            classify_discovery_failure(&auth),
            ("authentication", true, false)
        );

        let unavailable = anyhow::Error::new(gents::backend_provider::ModelDiscoveryHttpError {
            provider: "test".into(),
            url: "http://test/models".into(),
            status: 404,
            body: "missing".into(),
        });
        assert_eq!(
            classify_discovery_failure(&unavailable),
            ("http", true, true)
        );

        let disconnected = anyhow::anyhow!("connection refused");
        assert_eq!(
            classify_discovery_failure(&disconnected),
            ("connection", false, false)
        );
    }

    #[test]
    fn provider_account_view_never_serializes_tokens() {
        let credential = OAuthCredential {
            doc_id: Some("doc-1".to_string()),
            credential_id: "chatgpt-codex:did:key:zAgent".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            provider: "chatgpt-codex".to_string(),
            access_token: "secret-access".to_string(),
            refresh_token: "secret-refresh".to_string(),
            id_token: Some("secret-id".to_string()),
            account_id: Some("acct-1".to_string()),
            chatgpt_plan_type: Some("plus".to_string()),
            is_fedramp: false,
            access_token_expires_at: chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            last_refresh: None,
            enabled: true,
        };
        let json = serde_json::to_string(&ProviderAccountView::from(&credential)).unwrap();
        assert!(!json.contains("secret-access"));
        assert!(!json.contains("secret-refresh"));
        assert!(!json.contains("secret-id"));
        assert!(json.contains("acct-1"));
    }
}

#[tauri::command]
pub(crate) fn desktop_grok_login_cancel(
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    use std::sync::atomic::Ordering;

    let flag = {
        let mut bridge = state
            .bridge
            .lock()
            .map_err(|_| BridgeError::untyped("desktop bridge lock poisoned"))?;
        bridge.grok_login_cancel.take()
    };
    if let Some(flag) = flag {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeLoginRequest {
    pub agent_did: String,
    #[serde(default)]
    pub provider: Option<String>,
}

/// Redacted credential metadata for the webview (tokens never cross the bridge).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeLoginResult {
    pub doc_id: String,
    pub credential_id: String,
    pub agent_did: String,
    pub provider: String,
    pub access_token_expires_at: String,
    pub enabled: bool,
}

impl ClaudeLoginResult {
    fn redacted(doc_id: String, credential: &OAuthCredential) -> Self {
        Self {
            doc_id,
            credential_id: credential.credential_id.clone(),
            agent_did: credential.agent_did.clone(),
            provider: credential.provider.clone(),
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            enabled: credential.enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeLoginUrl {
    pub url: String,
}

#[tauri::command]
pub(crate) async fn desktop_claude_login<R: Runtime>(
    app: AppHandle<R>,
    request: ClaudeLoginRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ClaudeLoginResult, BridgeError> {
    use gents::claude_oauth::{
        credential_from_login_tokens, normalize_provider, ClaudeLoginTokens,
    };
    use gents_claude_login::{run_loopback_login, LoginOptions};

    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        return Err(BridgeError::untyped("agent_did is required"));
    }
    let provider = normalize_provider(request.provider.as_deref().unwrap_or_default());

    let server = run_loopback_login(LoginOptions::default())
        .map_err(|error| BridgeError::untyped(format!("starting Claude login server: {error}")))?;
    let _ = app.emit(
        crate::contract::CLAUDE_LOGIN_URL_EVENT,
        ClaudeLoginUrl {
            url: server.auth_url.clone(),
        },
    );

    let cancel = server.cancel_handle();
    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.claude_login_cancel = Some(cancel.clone());
    }
    let wait = tokio::time::timeout(CODEX_LOGIN_TIMEOUT, server.block_until_done()).await;
    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.claude_login_cancel = None;
    }
    let tokens = match wait {
        Ok(result) => result.map_err(|error| {
            BridgeError::untyped(format!("Claude browser login failed: {error}"))
        })?,
        Err(_elapsed) => {
            cancel.shutdown();
            return Err(BridgeError::untyped(
                "Claude sign-in timed out waiting for the browser",
            ));
        }
    };

    let login_tokens = ClaudeLoginTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_in: tokens.expires_in,
        scope: tokens.scope,
        account_id: tokens.account_id,
    };
    let credential =
        credential_from_login_tokens(&agent_did, &provider, &login_tokens, chrono::Utc::now());
    let access = core
        .operator_access(&agent_did)
        .map_err(|error| BridgeError::untyped(format!("storing Claude credential: {error}")))?;
    let doc_id = gents::oauth_credential::upsert_oauth_credential_on(&access, &credential)
        .await
        .map_err(|error| BridgeError::untyped(format!("storing Claude credential: {error}")))?;

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );

    Ok(ClaudeLoginResult::redacted(doc_id, &credential))
}

#[tauri::command]
pub(crate) fn desktop_claude_login_cancel(
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let handle = {
        let mut bridge = state
            .bridge
            .lock()
            .map_err(|_| BridgeError::untyped("desktop bridge lock poisoned"))?;
        bridge.claude_login_cancel.take()
    };
    if let Some(handle) = handle {
        handle.shutdown();
    }
    Ok(())
}
