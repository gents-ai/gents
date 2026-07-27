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

/// How long a probe waits before treating a local endpoint as unreachable.
/// A touch longer than the CLI's 700ms so a cold local server still answers,
/// but short enough that the wizard stays responsive.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Upper bound on how long a ChatGPT sign-in waits for the browser callback
/// before giving up, so an abandoned sign-in can never hang forever even if the
/// user never clicks Cancel.
const CODEX_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceProbeRequest {
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceProbeResult {
    pub reachable: bool,
    pub models: Vec<String>,
}

/// Probe an OpenAI-compatible endpoint for the models it advertises.
///
/// Mirrors the CLI init picker's local-server detection: a short-timeout
/// `GET {base}/models`, reading the advertised ids out of `data[].id`. Never
/// errors on an unreachable server — an offline local server is an expected,
/// non-exceptional answer the wizard renders as "not detected".
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexLoginRequest {
    pub agent_did: String,
    #[serde(default)]
    pub provider: Option<String>,
}

/// A redacted view of a stored credential. Tokens never cross the bridge into
/// the webview — only the metadata the UI needs to confirm the login worked.
#[derive(Debug, Clone, Serialize)]
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

/// Drive a ChatGPT/Codex OAuth login and persist the credential for `agentDid`.
///
/// Replicates `gents codex-login` against the desktop client's embedded node:
/// runs the loopback + PKCE login server (which opens the system browser),
/// reads the resulting tokens from codex's ephemeral in-process store, and
/// upserts a native `OAuthCredential` document. The runtime picks the document
/// up on reconcile and thereafter mints and auto-refreshes the Responses client
/// for any ChatGptCodex-preset backend on that DID.
///
/// The auth URL is emitted as `desktop://codex-login-url` before the flow
/// blocks, so the wizard can offer a manual-open fallback if the browser did
/// not launch on its own.
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

    // codex's ephemeral auth store is process-global and keyed by the codex
    // home path, so the login server (writer) and the AuthManager (reader) must
    // share this synthetic throwaway path within this one process. Nothing ever
    // touches the user's real ~/.codex.
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

    // Publish a cancel handle so `desktop_codex_login_cancel` (a closed browser,
    // an explicit Cancel) can abort the callback wait and free the loopback port.
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
            // Stop the still-listening server so its port is released, then bail.
            cancel.shutdown();
            return Err(BridgeError::from_legacy_message(
                "ChatGPT sign-in timed out waiting for the browser",
            ));
        }
    }

    let manager = AuthManager::new(
        synthetic_home,
        /* enable_codex_api_key_env */ false,
        AuthCredentialsStoreMode::Ephemeral,
        /* chatgpt_base_url */ None,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexLoginUrl {
    url: String,
}

/// Abort an in-flight ChatGPT/Codex sign-in (e.g. the user closed the browser).
///
/// Signals the login server to stop, which unblocks the pending
/// `desktop_codex_login` call — it then falls through to a "no auth" error and
/// returns. A no-op when no sign-in is in progress.
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
