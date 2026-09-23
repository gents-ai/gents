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

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{
    current_core, CredentialSave, DesktopAppState, IssuedOAuthCredential, PendingOAuthCredentials,
};
use crate::types::ClientUpdateEvent;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

const CODEX_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

const LOG_TARGET: &str = "gents_desktop_bridge::inference_setup";

/// User-facing name for a credential provider. Setup errors name the account
/// the user signed in to, never the runtime endpoint that failed.
fn provider_label(provider: &str) -> &'static str {
    match provider {
        gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER => "ChatGPT",
        gents::claude_oauth::CLAUDE_OAUTH_PROVIDER => "Claude",
        gents::xai_grok_oauth::XAI_OAUTH_PROVIDER => "Grok",
        _ => "provider",
    }
}

/// Refuses to start a browser sign-in unless the agent's canonical
/// configuration owner answers. The probe reads the agent's credentials through
/// the same access the save will use, so a runtime that is not serving is
/// reported before the user completes an OAuth flow whose tokens could not be
/// stored. Detail stays in the log; the returned message is user-facing.
async fn require_reachable_configuration(
    access: anyhow::Result<gents::ConfigAccess>,
    agent_did: &str,
    provider: &str,
) -> Result<(), BridgeError> {
    let probe = match access {
        Ok(access) => list_oauth_credentials_on(&access, agent_did)
            .await
            .map(|_| ()),
        Err(error) => Err(error),
    };
    probe.map_err(|error| {
        tracing::warn!(
            target: LOG_TARGET,
            agent_did,
            provider,
            error = %format!("{error:#}"),
            "agent configuration is not reachable; not starting provider sign-in"
        );
        BridgeError::new(
            BridgeErrorCode::EndpointUnreachable,
            format!(
                "The agent is not running, so {} sign-in was not started. Start the agent and try again.",
                provider_label(provider)
            ),
        )
    })
}

async fn upsert_through(
    access: anyhow::Result<gents::ConfigAccess>,
    credential: OAuthCredential,
) -> anyhow::Result<String> {
    gents::oauth_credential::upsert_oauth_credential_on(&access?, &credential).await
}

fn credential_not_saved(credential: &OAuthCredential, error: &anyhow::Error) -> BridgeError {
    tracing::warn!(
        target: LOG_TARGET,
        agent_did = %credential.agent_did,
        provider = %credential.provider,
        error = %format!("{error:#}"),
        "saving the issued provider credential failed; holding it for retry"
    );
    BridgeError::new(
        BridgeErrorCode::CredentialNotSaved,
        format!(
            "You are signed in to {}, but Gents could not save the sign-in to the agent. Make sure the agent is running, then retry saving.",
            provider_label(&credential.provider)
        ),
    )
}

/// Saves a credential issued by a completed sign-in through the agent's
/// canonical configuration owner. A failed save keeps the credential in the
/// bridge's in-memory pending set so `desktop_provider_account_retry_save` can
/// store it without another browser login.
async fn save_issued_credential(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    issued: IssuedOAuthCredential,
) -> Result<String, BridgeError> {
    let credential = issued.credential().clone();
    match pending
        .save(issued, |credential| upsert_through(access, credential))
        .await
    {
        CredentialSave::Saved(doc_id) => Ok(doc_id),
        CredentialSave::Superseded => Err(BridgeError::untyped(format!(
            "A newer {} sign-in for this agent replaced this one.",
            provider_label(&credential.provider)
        ))),
        CredentialSave::Failed(error) => Err(credential_not_saved(&credential, &error)),
    }
}

/// Retries the save of a held credential for exactly this agent and provider.
async fn retry_pending_credential(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    agent_did: &str,
    provider: &str,
) -> Result<OAuthCredential, BridgeError> {
    let (credential, saved) = pending
        .retry(agent_did, provider, |credential| {
            upsert_through(access, credential)
        })
        .await
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::NotFound,
                "There is no unsaved sign-in to retry. Sign in again.",
            )
        })?;
    match saved {
        Ok(_) => Ok(credential),
        Err(error) => Err(credential_not_saved(&credential, &error)),
    }
}

/// Stored provider accounts plus the redacted view of sign-ins the bridge
/// holds after a failed save. Held sign-ins are reported even when the store
/// cannot be read, since retrying them is the way back to a stored account.
async fn observe_provider_accounts(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    agent_did: &str,
) -> Result<Vec<ProviderAccountView>, BridgeError> {
    let held: Vec<ProviderAccountView> = pending
        .held_for(agent_did)
        .iter()
        .map(ProviderAccountView::pending_save)
        .collect();
    let stored = match access {
        Ok(access) => list_oauth_credentials_on(&access, agent_did).await,
        Err(error) => Err(error),
    };
    match stored {
        Ok(stored) => Ok(stored
            .iter()
            .map(ProviderAccountView::from)
            .chain(held)
            .collect()),
        Err(error) if !held.is_empty() => {
            tracing::warn!(
                target: LOG_TARGET,
                agent_did,
                error = %format!("{error:#}"),
                "reading stored provider accounts failed; reporting held sign-ins only"
            );
            Ok(held)
        }
        Err(error) => Err(BridgeError::untyped(error.to_string())),
    }
}

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
    pub max_context_window: Option<i64>,
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
    pub max_context_window: Option<i64>,
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
            max_context_window: request.max_context_window,
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
        max_context_window: request.max_context_window,
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
        let access = core.operator_access(request.agent_did.trim()).map_err(|error| {
            tracing::warn!(
                target: LOG_TARGET,
                agent_did = %request.agent_did.trim(),
                error = %format!("{error:#}"),
                "resolving agent configuration access for model discovery failed"
            );
            BridgeError::new(
                BridgeErrorCode::EndpointUnreachable,
                "The agent is not running, so connected accounts could not be checked. Start the agent and try again.",
            )
        })?;
        gents::oauth_credential::list_oauth_credentials_on(&access, request.agent_did.trim())
            .await
            .map_err(|error| {
                tracing::warn!(
                    target: LOG_TARGET,
                    agent_did = %request.agent_did.trim(),
                    error = %format!("{error:#}"),
                    "reading provider accounts for model discovery failed"
                );
                BridgeError::new(
                    BridgeErrorCode::EndpointUnreachable,
                    "The agent is not running, so connected accounts could not be checked. Start the agent and try again.",
                )
            })?
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
    require_reachable_configuration(core.operator_access(&agent_did), &agent_did, &provider)
        .await?;

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
    let doc_id = save_issued_credential(
        &state.pending_oauth_credentials,
        core.operator_access(&agent_did),
        state.pending_oauth_credentials.issue(credential.clone()),
    )
    .await?;

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
    /// A completed sign-in the bridge holds because saving it failed; it is
    /// not a stored account until `desktop_provider_account_retry_save`
    /// succeeds.
    pub pending_save: bool,
}

impl ProviderAccountView {
    fn pending_save(credential: &OAuthCredential) -> Self {
        Self {
            pending_save: true,
            ..Self::from(credential)
        }
    }
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
            pending_save: false,
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
    observe_provider_accounts(
        &state.pending_oauth_credentials,
        core.operator_access(agent_did),
        agent_did,
    )
    .await
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

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderAccountRetrySaveRequest {
    pub agent_did: String,
    /// Credential provider kind, e.g. `claude-subscription`.
    pub provider: String,
}

/// Saves a credential that a completed sign-in issued but could not store,
/// without repeating the browser login. Only the redacted account view
/// crosses the bridge.
#[tauri::command]
pub(crate) async fn desktop_provider_account_retry_save<R: Runtime>(
    app: AppHandle<R>,
    request: ProviderAccountRetrySaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ProviderAccountView, BridgeError> {
    let core = current_core(&state).ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientNotRunning,
            "desktop client is not running",
        )
    })?;
    let agent_did = request.agent_did.trim();
    let provider = request.provider.trim();
    let credential = retry_pending_credential(
        &state.pending_oauth_credentials,
        core.operator_access(agent_did),
        agent_did,
        provider,
    )
    .await?;
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
    Ok(ProviderAccountView::from(&credential))
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
    require_reachable_configuration(core.operator_access(&agent_did), &agent_did, &provider)
        .await?;

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
            if let Err(error) = crate::host_browser::open_url(url) {
                tracing::warn!(%error, "could not open the Grok sign-in page; the page link stands in");
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
    let doc_id = save_issued_credential(
        &state.pending_oauth_credentials,
        core.operator_access(&agent_did),
        state.pending_oauth_credentials.issue(credential.clone()),
    )
    .await?;

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

    fn issued_credential(agent_did: &str) -> OAuthCredential {
        OAuthCredential {
            doc_id: None,
            credential_id: format!("claude-subscription:{agent_did}"),
            agent_did: agent_did.to_string(),
            provider: gents::claude_oauth::CLAUDE_OAUTH_PROVIDER.to_string(),
            access_token: "issued-access".to_string(),
            refresh_token: "issued-refresh".to_string(),
            id_token: None,
            account_id: Some("acct-1".to_string()),
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            last_refresh: None,
            enabled: true,
        }
    }

    /// Operator GraphQL access for a runtime that is not serving.
    fn unreachable_operator() -> gents::ConfigAccess {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
        let address = listener.local_addr().expect("local address");
        drop(listener);
        gents::ConfigAccess::Graphql(format!("http://{address}/api/v0/graphql"))
    }

    async fn serving_node() -> std::sync::Arc<gents::defra_node::EmbeddedNode> {
        let node = gents::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .expect("embedded node");
        gents::ensure_runtime_schemas(&node).await.expect("schemas");
        std::sync::Arc::new(node)
    }

    async fn serving_operator() -> gents::ConfigAccess {
        gents::ConfigAccess::Local(serving_node().await)
    }

    fn assert_user_facing(error: &BridgeError) {
        let message = error.message.to_ascii_lowercase();
        for internal in ["127.0.0.1", "http://", "graphql", "/api/v0"] {
            assert!(
                !message.contains(internal),
                "setup error leaks {internal:?}: {}",
                error.message
            );
        }
        assert!(error.endpoint.is_none());
    }

    #[tokio::test]
    async fn sign_in_is_refused_before_oauth_when_the_runtime_is_not_serving() {
        let error = require_reachable_configuration(
            Ok(unreachable_operator()),
            "did:key:zAgent",
            gents::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        )
        .await
        .expect_err("an unreachable runtime must refuse to start sign-in");
        assert_eq!(error.code, BridgeErrorCode::EndpointUnreachable);
        assert!(error.message.contains("Claude sign-in was not started"));
        assert_user_facing(&error);

        let missing_endpoint = require_reachable_configuration(
            Err(anyhow::anyhow!(
                "managed agent did:key:zAgent has no operator GraphQL endpoint"
            )),
            "did:key:zAgent",
            gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        )
        .await
        .expect_err("unresolvable configuration access must refuse sign-in");
        assert_eq!(missing_endpoint.code, BridgeErrorCode::EndpointUnreachable);
        assert_user_facing(&missing_endpoint);

        require_reachable_configuration(
            Ok(serving_operator().await),
            "did:key:zAgent",
            gents::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        )
        .await
        .expect("a serving runtime admits sign-in");
    }

    fn held_tokens(pending: &PendingOAuthCredentials, agent_did: &str) -> Vec<String> {
        pending
            .held_for(agent_did)
            .into_iter()
            .map(|credential| credential.access_token)
            .collect()
    }

    fn credential_with_token(agent_did: &str, token: &str) -> OAuthCredential {
        OAuthCredential {
            access_token: token.to_string(),
            ..issued_credential(agent_did)
        }
    }

    #[tokio::test]
    async fn failed_save_holds_the_issued_credential_and_retry_saves_it_without_login() {
        let pending = PendingOAuthCredentials::default();
        let agent = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let error = save_issued_credential(
            &pending,
            Ok(unreachable_operator()),
            pending.issue(issued_credential(agent)),
        )
        .await
        .expect_err("a runtime that is not serving cannot store the credential");
        assert_eq!(error.code, BridgeErrorCode::CredentialNotSaved);
        assert!(error.retryable);
        assert!(error.message.contains("signed in to Claude"));
        assert_user_facing(&error);
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains("issued-access"));
        assert!(!serialized.contains("issued-refresh"));
        assert_eq!(held_tokens(&pending, agent), ["issued-access"]);

        // A retry while the runtime is still down keeps the credential held.
        let still_down = retry_pending_credential(
            &pending,
            Err(anyhow::anyhow!("no operator GraphQL endpoint")),
            agent,
            provider,
        )
        .await
        .expect_err("retry against an unavailable runtime fails");
        assert_eq!(still_down.code, BridgeErrorCode::CredentialNotSaved);
        assert_user_facing(&still_down);
        assert_eq!(held_tokens(&pending, agent), ["issued-access"]);

        // Once the canonical owner serves, retry stores the same tokens.
        let node = serving_node().await;
        let saved = retry_pending_credential(
            &pending,
            Ok(gents::ConfigAccess::Local(node.clone())),
            agent,
            provider,
        )
        .await
        .expect("retry stores the held credential");
        assert_eq!(saved.access_token, "issued-access");
        assert!(held_tokens(&pending, agent).is_empty());
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::Local(node), agent)
            .await
            .expect("list stored credentials");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].provider, provider);
        assert_eq!(stored[0].access_token, "issued-access");
        assert_eq!(stored[0].refresh_token, "issued-refresh");
    }

    #[tokio::test]
    async fn account_observation_reports_held_sign_ins_without_tokens() {
        let pending = PendingOAuthCredentials::default();
        let agent = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let unobserved = observe_provider_accounts(&pending, Ok(unreachable_operator()), agent)
            .await
            .expect_err("an unreadable store with nothing held stays an error");
        assert_eq!(unobserved.code, BridgeErrorCode::Unknown);

        let _ = save_issued_credential(
            &pending,
            Ok(unreachable_operator()),
            pending.issue(issued_credential(agent)),
        )
        .await;
        let held = observe_provider_accounts(&pending, Ok(unreachable_operator()), agent)
            .await
            .expect("held sign-ins are observable while the store is unreachable");
        assert_eq!(held.len(), 1);
        assert!(held[0].pending_save);
        assert_eq!(held[0].provider, provider);
        let json = serde_json::to_string(&held).unwrap();
        assert!(!json.contains("issued-access"));
        assert!(!json.contains("issued-refresh"));
        assert!(
            observe_provider_accounts(&pending, Ok(unreachable_operator()), "did:key:zOther")
                .await
                .is_err()
        );

        let node = serving_node().await;
        let with_store = observe_provider_accounts(
            &pending,
            Ok(gents::ConfigAccess::Local(node.clone())),
            agent,
        )
        .await
        .expect("serving store");
        assert_eq!(with_store.len(), 1);
        assert!(with_store[0].pending_save);

        retry_pending_credential(
            &pending,
            Ok(gents::ConfigAccess::Local(node.clone())),
            agent,
            provider,
        )
        .await
        .expect("retry stores the held credential");
        let saved =
            observe_provider_accounts(&pending, Ok(gents::ConfigAccess::Local(node)), agent)
                .await
                .expect("serving store");
        assert_eq!(saved.len(), 1);
        assert!(!saved[0].pending_save);
        assert_eq!(saved[0].provider, provider);
    }

    #[tokio::test]
    async fn retry_save_only_uses_a_credential_held_for_that_agent_and_provider() {
        let pending = PendingOAuthCredentials::default();
        let _ = save_issued_credential(
            &pending,
            Ok(unreachable_operator()),
            pending.issue(issued_credential("did:key:zAgent")),
        )
        .await;

        for (agent, provider) in [
            ("did:key:zOther", gents::claude_oauth::CLAUDE_OAUTH_PROVIDER),
            (
                "did:key:zAgent",
                gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            ),
        ] {
            let error =
                retry_pending_credential(&pending, Ok(serving_operator().await), agent, provider)
                    .await
                    .expect_err("no credential is held for this key");
            assert_eq!(error.code, BridgeErrorCode::NotFound);
        }
        assert_eq!(held_tokens(&pending, "did:key:zAgent"), ["issued-access"]);
    }

    type Writes = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    fn failed_write(_: OAuthCredential) -> std::future::Ready<anyhow::Result<()>> {
        std::future::ready(Err(anyhow::anyhow!("configuration owner unavailable")))
    }

    #[tokio::test]
    async fn an_in_flight_retry_of_an_older_sign_in_cannot_discard_a_newer_held_one() {
        let pending = std::sync::Arc::new(PendingOAuthCredentials::default());
        let writes = Writes::default();
        let agent = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let older = pending.issue(credential_with_token(agent, "token-a"));
        assert!(matches!(
            pending.save(older, failed_write).await,
            CredentialSave::Failed(_)
        ));

        let (older_started, older_writing) = tokio::sync::oneshot::channel::<()>();
        let (release_older, older_released) = tokio::sync::oneshot::channel::<()>();
        let retry_older = tokio::spawn({
            let pending = pending.clone();
            let writes = writes.clone();
            async move {
                pending
                    .retry(agent, provider, move |credential| async move {
                        let _ = older_started.send(());
                        let _ = older_released.await;
                        writes.lock().unwrap().push(credential.access_token);
                        anyhow::Ok(())
                    })
                    .await
                    .map(|(_, saved)| saved.is_ok())
            }
        });
        older_writing.await.expect("older retry is writing");

        let newer = pending.issue(credential_with_token(agent, "token-b"));
        let (newer_started, mut newer_writing) = tokio::sync::oneshot::channel::<()>();
        let save_newer = tokio::spawn({
            let pending = pending.clone();
            async move {
                matches!(
                    pending
                        .save(newer, move |credential| {
                            let _ = newer_started.send(());
                            failed_write(credential)
                        })
                        .await,
                    CredentialSave::Failed(_)
                )
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            newer_writing.try_recv().is_err(),
            "the newer save must wait for the older write"
        );

        release_older.send(()).expect("release older write");
        assert_eq!(retry_older.await.unwrap(), Some(true));
        assert!(save_newer.await.unwrap());
        assert_eq!(held_tokens(&pending, agent), ["token-b"]);
        assert_eq!(*writes.lock().unwrap(), ["token-a"]);

        let retried = pending
            .retry(agent, provider, {
                let writes = writes.clone();
                move |credential| async move {
                    writes.lock().unwrap().push(credential.access_token);
                    anyhow::Ok(())
                }
            })
            .await
            .map(|(credential, saved)| (credential.access_token, saved.is_ok()));
        assert_eq!(retried, Some(("token-b".to_string(), true)));
        assert!(held_tokens(&pending, agent).is_empty());
        assert_eq!(*writes.lock().unwrap(), ["token-a", "token-b"]);
    }

    #[tokio::test]
    async fn an_older_sign_in_saved_after_a_newer_one_never_overwrites_it() {
        let pending = std::sync::Arc::new(PendingOAuthCredentials::default());
        let writes = Writes::default();
        let agent = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let older = pending.issue(credential_with_token(agent, "token-a"));
        let newer = pending.issue(credential_with_token(agent, "token-b"));

        let (newer_started, newer_writing) = tokio::sync::oneshot::channel::<()>();
        let (release_newer, newer_released) = tokio::sync::oneshot::channel::<()>();
        let save_newer = tokio::spawn({
            let pending = pending.clone();
            let writes = writes.clone();
            async move {
                matches!(
                    pending
                        .save(newer, move |credential| async move {
                            let _ = newer_started.send(());
                            let _ = newer_released.await;
                            writes.lock().unwrap().push(credential.access_token);
                            anyhow::Ok(())
                        })
                        .await,
                    CredentialSave::Saved(())
                )
            }
        });
        newer_writing.await.expect("newer save is writing");

        let save_older = tokio::spawn({
            let pending = pending.clone();
            let writes = writes.clone();
            async move {
                matches!(
                    pending
                        .save(older, move |credential| async move {
                            writes.lock().unwrap().push(credential.access_token);
                            anyhow::Ok(())
                        })
                        .await,
                    CredentialSave::Superseded
                )
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        release_newer.send(()).expect("release newer write");
        assert!(save_newer.await.unwrap());
        assert!(save_older.await.unwrap(), "the older save is superseded");
        assert_eq!(*writes.lock().unwrap(), ["token-b"]);

        let failed_older = pending.issue(credential_with_token(agent, "token-c"));
        let saved_newer = pending.issue(credential_with_token(agent, "token-d"));
        assert!(matches!(
            pending.save(failed_older, failed_write).await,
            CredentialSave::Failed(_)
        ));
        assert!(matches!(
            pending
                .save(saved_newer, {
                    let writes = writes.clone();
                    move |credential| async move {
                        writes.lock().unwrap().push(credential.access_token);
                        anyhow::Ok(())
                    }
                })
                .await,
            CredentialSave::Saved(())
        ));
        assert!(held_tokens(&pending, agent).is_empty());
        assert!(pending.retry(agent, provider, failed_write).await.is_none());
        assert_eq!(*writes.lock().unwrap(), ["token-b", "token-d"]);
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
    require_reachable_configuration(core.operator_access(&agent_did), &agent_did, &provider)
        .await?;

    let server = run_loopback_login(LoginOptions {
        // Opened here instead: a packaged build has to strip its own
        // environment before handing a URL to a host program.
        open_browser: false,
        ..LoginOptions::default()
    })
    .map_err(|error| BridgeError::untyped(format!("starting Claude login server: {error}")))?;
    let _ = app.emit(
        crate::contract::CLAUDE_LOGIN_URL_EVENT,
        ClaudeLoginUrl {
            url: server.auth_url.clone(),
        },
    );
    if let Err(error) = crate::host_browser::open_url(&server.auth_url) {
        tracing::warn!(%error, "could not open the Claude sign-in page; the page link stands in");
    }

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
    let doc_id = save_issued_credential(
        &state.pending_oauth_credentials,
        core.operator_access(&agent_did),
        state.pending_oauth_credentials.issue(credential.clone()),
    )
    .await?;

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
