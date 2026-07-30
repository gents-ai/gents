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

use codex_login::{
    run_login_server, AuthCredentialsStoreMode, AuthManager, ServerOptions, CLIENT_ID,
};
use gents::chatgpt_codex::{normalize_provider, upsert_oauth_credential, OAuthCredential};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime, State};
use uuid::Uuid;

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
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        return Err(BridgeError::from_legacy_message("agent_did is required"));
    }
    let provider = normalize_provider(request.provider.as_deref().unwrap_or_default());

    let synthetic_home = std::env::temp_dir().join(format!("gents-codex-login-{}", Uuid::new_v4()));
    let server_opts = ServerOptions::new(
        synthetic_home.clone(),
        CLIENT_ID.to_string(),
        None,
        AuthCredentialsStoreMode::Ephemeral,
    );

    let server = run_login_server(server_opts).map_err(|error| {
        BridgeError::from_legacy_message(format!("starting ChatGPT login server: {error}"))
    })?;
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
    match wait {
        Ok(result) => {
            result.map_err(|error| {
                BridgeError::from_legacy_message(format!("ChatGPT browser login failed: {error}"))
            })?;
        }
        Err(_elapsed) => {
            cancel.shutdown();
            return Err(BridgeError::from_legacy_message(
                "ChatGPT sign-in timed out waiting for the browser",
            ));
        }
    }

    let manager = AuthManager::new(
        synthetic_home,
        false,
        AuthCredentialsStoreMode::Ephemeral,
        None,
    )
    .await;
    let auth = manager
        .auth()
        .await
        .ok_or_else(|| "ChatGPT login completed but no auth was returned".to_string())?;
    if !auth.is_chatgpt_auth() {
        return Err(BridgeError::from_legacy_message(format!(
            "ChatGPT login returned {:?}; ChatGPT OAuth credentials are required",
            auth.auth_mode()
        )));
    }
    let token_data = auth.get_token_data().map_err(|error| {
        BridgeError::from_legacy_message(format!(
            "ChatGPT login did not expose token data: {error}"
        ))
    })?;

    let credential = OAuthCredential::from_login_token_data(
        &agent_did,
        &provider,
        &token_data,
        chrono::Utc::now(),
    );
    let node = core.node_arc();
    let doc_id = upsert_oauth_credential(&node, &credential)
        .await
        .map_err(|error| {
            BridgeError::from_legacy_message(format!("storing ChatGPT credential: {error}"))
        })?;

    // Storing the credential is exactly the signal the runtime reconciles on to
    // flip a ChatGptCodex behavior available; nudge the UI to refetch health.
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent { reason: "config" },
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
            .map_err(|_| BridgeError::from_legacy_message("desktop bridge lock poisoned"))?;
        bridge.codex_login_cancel.take()
    };
    if let Some(handle) = handle {
        handle.shutdown();
    }
    Ok(())
}
