//! First-party inference onboarding commands for the desktop app.
//!
//! These back the guided "set up inference" wizard: a local-server probe that
//! mirrors the CLI init picker's `GET {base}/models` auto-detection, and a
//! first-party ChatGPT/Codex OAuth login that replicates `gents codex-login`
//! against the desktop client's embedded node. The backend documents for the
//! OpenAI / local / custom options are written through the existing
//! `desktop_backend_save` command; this module owns only the two pieces that
//! have no equivalent yet.

use std::time::Duration;

use gents::chatgpt_codex::normalize_provider;
use gents::oauth_credential::{list_oauth_credentials, upsert_oauth_credential, OAuthCredential};
use gents_chatgpt_login::{run_login_server, LoginOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::error::BridgeError;
use crate::state::{current_core, DesktopAppState};
use crate::types::ClientUpdateEvent;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

const CODEX_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

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
    let node = core.node_arc();
    let doc_id = upsert_oauth_credential(&node, &credential)
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
            account_id: credential.account_id.clone(),
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
    let credentials = list_oauth_credentials(core.node(), agent_did)
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
    let credentials = list_oauth_credentials(core.node(), request.agent_did.trim())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let mut credential = credentials
        .into_iter()
        .find(|entry| entry.credential_id == request.credential_id)
        .ok_or_else(|| BridgeError::untyped("provider account not found"))?;
    credential.enabled = false;
    upsert_oauth_credential(core.node(), &credential)
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

    use gents::oauth_credential::upsert_oauth_credential;
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
    let node = core.node_arc();
    let doc_id = upsert_oauth_credential(&node, &credential)
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
