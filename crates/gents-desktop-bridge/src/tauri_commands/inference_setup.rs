//! First-party inference onboarding commands for the desktop app.
//!
//! Rust owns the versioned provider catalog, canonical connection mapping,
//! advertised-model discovery, and Gents recommendations. OAuth credentials
//! remain node-scoped documents and never cross this bridge unredacted. The UI
//! commits the chosen canonical components through the existing atomic operator
//! configuration command.

use std::time::Duration;

use gents::chatgpt_codex::normalize_provider;
use gents::inference_setup::{
    InferenceAuthMethod, InferenceModelOption, InferenceModelRecommendation, InferenceProviderId,
    InferenceSetupCatalog,
};
use gents::oauth_credential::{list_oauth_credentials_on, OAuthCredential, SignIn, SignInResult};
use gents_chatgpt_login::{run_login_server, LoginOptions};
use gents_desktop_core::client::ClientCore;
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

/// Refuses to start a browser sign-in unless the node's canonical
/// configuration owner answers. The probe reads the node's credentials through
/// the same access the save will use, so a runtime that is not serving is
/// reported before the user completes an OAuth flow whose tokens could not be
/// stored. Detail stays in the log; the returned message is user-facing.
async fn require_reachable_configuration(
    access: anyhow::Result<gents::ConfigAccess>,
    node_did: &str,
    provider: &str,
) -> Result<(), BridgeError> {
    let probe = match access {
        Ok(access) => list_oauth_credentials_on(&access, node_did)
            .await
            .map(|_| ()),
        Err(error) => Err(error),
    };
    probe.map_err(|error| {
        tracing::warn!(
            target: LOG_TARGET,
            node_did,
            provider,
            error = %format!("{error:#}"),
            "node configuration is not reachable; not starting provider sign-in"
        );
        BridgeError::new(
            BridgeErrorCode::EndpointUnreachable,
            format!(
                "The node is not running, so {} sign-in was not started. Start the node and try again.",
                provider_label(provider)
            ),
        )
    })
}

/// Stores a sign-in; its label names the account only when the sign-in adds
/// one, since a desktop label is typed for a new account and must never
/// rename the stored account the browser happened to return. The account is
/// stored before its label is set, so a label that fails keeps the default
/// one rather than reporting a stored account as unsaved.
async fn upsert_through(
    access: anyhow::Result<gents::ConfigAccess>,
    credential: OAuthCredential,
) -> anyhow::Result<SignIn> {
    let access = access?;
    let label = credential.label.clone();
    let mut signed = gents::oauth_credential::store_sign_in(&access, credential, None).await?;
    if let (SignInResult::Added, Some(label)) = (&signed.result, label) {
        let stored = &signed.credential;
        match gents::oauth_credential::set_account_label(
            &access,
            &stored.node_did,
            &stored.credential_id,
            &label,
        )
        .await
        {
            Ok(()) => signed.credential.label = Some(label),
            Err(error) => tracing::warn!(
                target: LOG_TARGET,
                node_did = %stored.node_did,
                provider = %stored.provider,
                error = %format!("{error:#}"),
                "labelling the added account failed; it keeps its default label"
            ),
        }
    }
    Ok(signed)
}

/// The label a sign-in request names: trimmed, empty is none, and checked
/// before the browser opens so a bad label never costs a sign-in.
fn sign_in_label(label: Option<&str>) -> Result<Option<String>, BridgeError> {
    label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(gents::oauth_credential::validate_account_label)
        .transpose()
        .map_err(|error| BridgeError::new(BridgeErrorCode::InvalidArgument, error.to_string()))
}

fn credential_not_saved(credential: &OAuthCredential, error: &anyhow::Error) -> BridgeError {
    tracing::warn!(
        target: LOG_TARGET,
        node_did = %credential.node_did,
        provider = %credential.provider,
        error = %format!("{error:#}"),
        "saving the issued provider credential failed; holding it for retry"
    );
    BridgeError::new(
        BridgeErrorCode::CredentialNotSaved,
        format!(
            "You are signed in to {}, but Gents could not save the sign-in to the node. Make sure the node is running, then retry saving.",
            provider_label(&credential.provider)
        ),
    )
}

/// Saves a credential issued by a completed sign-in through the node's
/// canonical configuration owner. A failed save keeps the credential in the
/// bridge's in-memory pending set so `desktop_provider_account_retry_save` can
/// store it without another browser login.
async fn save_issued_credential(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    issued: IssuedOAuthCredential,
) -> Result<SignIn, BridgeError> {
    let credential = issued.credential().clone();
    match pending
        .save(issued, |credential| upsert_through(access, credential))
        .await
    {
        CredentialSave::Saved(signed) => Ok(signed),
        CredentialSave::Superseded => Err(BridgeError::untyped(format!(
            "A newer {} sign-in for this node replaced this one.",
            provider_label(&credential.provider)
        ))),
        CredentialSave::Failed(error) => Err(credential_not_saved(&credential, &error)),
    }
}

/// Retries the save of a held credential for exactly this node and provider.
async fn retry_pending_credential(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    node_did: &str,
    provider: &str,
) -> Result<OAuthCredential, BridgeError> {
    let (credential, saved) = pending
        .retry(node_did, provider, |credential| {
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
        Ok(signed) => Ok(signed.credential),
        Err(error) => Err(credential_not_saved(&credential, &error)),
    }
}

/// Refreshes the client store, then tells the webview the node's
/// configuration changed. A write through the runtime's operator GraphQL never
/// reaches the desktop node, so the snapshot shows it only after a refresh. A
/// failed refresh is logged and never fails a write that is already stored.
async fn notify_config_changed<R: Runtime>(app: &AppHandle<R>, core: &ClientCore) {
    if let Err(error) = core.refresh_store().await {
        tracing::warn!(
            target: LOG_TARGET,
            error = %format!("{error:#}"),
            "refreshing the client store after a configuration write failed"
        );
    }
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
}

/// Stored provider accounts plus the redacted view of sign-ins the bridge
/// holds after a failed save. Held sign-ins are reported even when the store
/// cannot be read, since retrying them is the way back to a stored account.
async fn observe_provider_accounts(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    node_did: &str,
) -> Result<Vec<ProviderAccountView>, BridgeError> {
    let held: Vec<ProviderAccountView> = pending
        .held_for(node_did)
        .iter()
        .map(ProviderAccountView::pending_save)
        .collect();
    let stored = match access {
        Ok(access) => list_oauth_credentials_on(&access, node_did).await,
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
                node_did,
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
pub struct InferenceDiscoveryFailure {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct InferenceDiscoveryRequest {
    pub request_key: String,
    pub node_did: String,
    pub provider: InferenceProviderId,
    pub auth_method: InferenceAuthMethod,
    pub endpoint: String,
    /// Ephemeral connection input. It is consumed for this request and is
    /// never reflected into a response or desktop snapshot.
    pub api_key: Option<String>,
    /// The account a subscription backend names; absent is the provider's
    /// original account.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub account_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct InferenceDiscoveryResult {
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
pub struct InferenceRecommendationRequest {
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
pub struct InferenceBackendRecommendationRequest {
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
    inference_model_recommendation(request)
}

pub fn inference_model_recommendation(
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
    inference_backend_recommendation(request)
}

pub fn inference_backend_recommendation(
    request: InferenceBackendRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    let (provider, auth_method) = gents::inference_setup::provider_selection_for_backend(
        request.provider_kind,
        &request.endpoint,
    );
    inference_model_recommendation(InferenceRecommendationRequest {
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
    let core = current_core(&state);
    discover_inference_models_for_core(request, core.as_deref()).await
}

pub async fn discover_inference_models_for_core(
    request: InferenceDiscoveryRequest,
    core: Option<&gents_desktop_core::client::ClientCore>,
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

    let (credential, oauth_auth) = if let Some(provider) = spec.oauth_provider {
        let core = core.ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
        let access = core.operator_access(request.node_did.trim()).map_err(|error| {
            tracing::warn!(
                target: LOG_TARGET,
                node_did = %request.node_did.trim(),
                error = %format!("{error:#}"),
                "resolving node configuration access for model discovery failed"
            );
            BridgeError::new(
                BridgeErrorCode::EndpointUnreachable,
                "The node is not running, so connected accounts could not be checked. Start the node and try again.",
            )
        })?;
        let (credential, auth) = discovery_account(
            &access,
            request.node_did.trim(),
            provider,
            request.account_ref.as_deref(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(
                target: LOG_TARGET,
                node_did = %request.node_did.trim(),
                error = %format!("{error:#}"),
                "reading provider accounts for model discovery failed"
            );
            BridgeError::new(
                BridgeErrorCode::EndpointUnreachable,
                "The node is not running, so connected accounts could not be checked. Start the node and try again.",
            )
        })?;
        (credential, Some(auth))
    } else {
        (None, None)
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

    if let (Ok(models), Some(core)) = (&discovered, core) {
        publish_discovered_catalog(core, &request, &spec, api_key, oauth_auth, models.clone())
            .await;
    }

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

/// The account a discovery for a backend naming `account_ref` reads, and the
/// auth its catalog is published under.
async fn discovery_account(
    access: &gents::ConfigAccess,
    node_did: &str,
    provider: &str,
    account_ref: Option<&str>,
) -> anyhow::Result<(Option<OAuthCredential>, gents::document_config::BackendAuth)> {
    let credential = gents::oauth_credential::resolve_oauth_credential(
        access,
        node_did,
        provider,
        gents::oauth_credential::AccountPick::Reference(account_ref),
    )
    .await?;
    Ok((
        credential,
        gents::document_config::BackendAuth::NodeOAuth {
            account_ref: account_ref.map(str::to_owned),
        },
    ))
}

/// Operator discovery is published as the credential-free catalog of every
/// persisted backend already using this exact connection, so self-config can
/// read it. Setup runs discovery again after persisting to publish it. A
/// failed publication never fails discovery; the previous catalog is kept.
async fn publish_discovered_catalog(
    core: &gents_desktop_core::client::ClientCore,
    request: &InferenceDiscoveryRequest,
    spec: &gents::inference_setup::InferenceConnectionSpec,
    api_key: Option<&str>,
    oauth_auth: Option<gents::document_config::BackendAuth>,
    models: Vec<gents::document_config::AdvertisedModel>,
) {
    use gents::document_config::BackendAuth;
    let node_did = request.node_did.trim();
    let auth = match (oauth_auth, api_key) {
        (Some(auth), _) => auth,
        (None, Some(key)) => BackendAuth::ApiKey {
            key: key.to_string(),
        },
        (None, None) => BackendAuth::Unauthenticated,
    };
    let result = match core.operator_access(node_did) {
        Ok(access) => {
            gents::backend_registry::record_connection_catalog_on(
                &access,
                node_did,
                spec.provider_kind,
                &spec.endpoint,
                &auth,
                models,
            )
            .await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(
            target: LOG_TARGET,
            node_did,
            error = %format!("{error:#}"),
            "publishing the discovered model catalog failed; the previous catalog is kept"
        );
    }
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
    pub node_did: String,
    #[serde(default)]
    pub provider: Option<String>,
    /// The new account's label; empty or absent leaves the store's default.
    #[serde(default)]
    pub label: Option<String>,
}

/// A redacted view of a stored credential. Tokens never cross the bridge into
/// the webview — only the metadata the UI needs to confirm the login worked.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexLoginResult {
    pub doc_id: String,
    pub credential_id: String,
    pub node_did: String,
    pub provider: String,
    pub account_id: Option<String>,
    pub chatgpt_plan_type: Option<String>,
    pub is_fedramp: bool,
    pub access_token_expires_at: String,
    pub enabled: bool,
    pub sign_in: SignInView,
}

impl CodexLoginResult {
    fn redacted(signed: &SignIn) -> Self {
        let credential = &signed.credential;
        Self {
            doc_id: signed.doc_id.clone(),
            credential_id: credential.credential_id.clone(),
            node_did: credential.node_did.clone(),
            provider: credential.provider.clone(),
            account_id: credential.account_id.clone(),
            chatgpt_plan_type: credential.chatgpt_plan_type.clone(),
            is_fedramp: credential.is_fedramp,
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            enabled: credential.enabled,
            sign_in: SignInView::from(signed),
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
    let node_did = request.node_did.trim().to_string();
    if node_did.is_empty() {
        return Err(BridgeError::untyped("node_did is required"));
    }
    let label = sign_in_label(request.label.as_deref())?;
    let provider = normalize_provider(request.provider.as_deref().unwrap_or_default());
    require_reachable_configuration(core.operator_access(&node_did), &node_did, &provider).await?;

    let server = run_login_server(LoginOptions {
        open_browser: false,
        ..LoginOptions::default()
    })
    .map_err(|error| BridgeError::untyped(format!("starting ChatGPT login server: {error}")))?;
    let _ = app.emit(
        "desktop://codex-login-url",
        CodexLoginUrl {
            url: server.auth_url.clone(),
        },
    );
    if let Err(error) = crate::host_browser::open_url(&server.auth_url) {
        tracing::warn!(%error, "could not open the ChatGPT sign-in page; the page link stands in");
    }

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

    let credential = OAuthCredential {
        label,
        ..OAuthCredential::from_login_tokens(
            &node_did,
            &provider,
            &tokens.id_token,
            tokens.access_token,
            tokens.refresh_token,
            chrono::Utc::now(),
        )
    };
    let signed = save_issued_credential(
        &state.pending_oauth_credentials,
        core.operator_access(&node_did),
        state.pending_oauth_credentials.issue(credential.clone()),
    )
    .await?;

    // Storing the credential is exactly the signal the runtime reconciles on to
    // flip the ChatGptCodex agent's readiness to available; nudge the UI to
    // refetch health.
    notify_config_changed(&app, &core).await;

    Ok(CodexLoginResult::redacted(&signed))
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
    pub node_did: String,
    #[serde(default)]
    pub provider: Option<String>,
    /// The new account's label; empty or absent leaves the store's default.
    #[serde(default)]
    pub label: Option<String>,
}

/// Redacted credential metadata for the webview (tokens never cross the bridge).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrokLoginResult {
    pub doc_id: String,
    pub credential_id: String,
    pub node_did: String,
    pub provider: String,
    pub access_token_expires_at: String,
    pub enabled: bool,
    pub sign_in: SignInView,
}

impl GrokLoginResult {
    fn redacted(signed: &SignIn) -> Self {
        let credential = &signed.credential;
        Self {
            doc_id: signed.doc_id.clone(),
            credential_id: credential.credential_id.clone(),
            node_did: credential.node_did.clone(),
            provider: credential.provider.clone(),
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            enabled: credential.enabled,
            sign_in: SignInView::from(signed),
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
    pub node_did: String,
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
    /// Which of the provider's accounts this is; `None` is the original
    /// account, which a backend without a reference runs on.
    pub account_ref: Option<String>,
    pub label: String,
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
            node_did: credential.node_did.clone(),
            provider: credential.provider.clone(),
            account_id: gents::oauth_credential::account_display_label(credential),
            plan_type: credential.chatgpt_plan_type.clone(),
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            last_refresh: credential.last_refresh.map(|value| value.to_rfc3339()),
            enabled: credential.enabled,
            pending_save: false,
            account_ref: credential.account_ref.clone(),
            label: gents::oauth_credential::effective_account_label(credential),
        }
    }
}

/// How a desktop sign-in was stored: a new account or a refresh of one
/// already stored, the account's label and reference, and after a refresh how
/// to reach another account.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SignInView {
    /// `added` or `refreshed`.
    pub result: String,
    pub label: String,
    pub account_ref: Option<String>,
    pub hint: Option<String>,
}

impl From<&SignIn> for SignInView {
    fn from(signed: &SignIn) -> Self {
        Self {
            result: match signed.result {
                SignInResult::Added => "added",
                SignInResult::Refreshed => "refreshed",
            }
            .to_string(),
            label: gents::oauth_credential::effective_account_label(&signed.credential),
            account_ref: signed.credential.account_ref.clone(),
            hint: signed.account_chooser_hint(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountsRequest {
    pub node_did: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountDisconnectRequest {
    pub node_did: String,
    pub credential_id: String,
}

#[tauri::command]
pub(crate) async fn desktop_provider_accounts_list(
    request: ProviderAccountsRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<ProviderAccountView>, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let node_did = request.node_did.trim();
    observe_provider_accounts(
        &state.pending_oauth_credentials,
        core.operator_access(node_did),
        node_did,
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
        .operator_access(request.node_did.trim())
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    gents::oauth_credential::set_account_enabled(
        &access,
        request.node_did.trim(),
        &request.credential_id,
        false,
    )
    .await
    .map_err(|error| BridgeError::untyped(error.to_string()))?;
    notify_config_changed(&app, &core).await;
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountRenameRequest {
    pub node_did: String,
    pub credential_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountRemoveRequest {
    pub node_did: String,
    pub credential_id: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderUsageReadRequest {
    pub node_did: String,
    /// `false` when the panel opens, `true` for an explicit Refresh. Both
    /// skip accounts read in the last five minutes; only a failed Refresh is
    /// an error.
    pub refresh: bool,
    /// Only this credential provider's accounts, e.g. `claude-subscription`.
    pub provider: Option<String>,
}

/// One backend's stored usage as the panel draws it: visible windows, or the
/// note saying why there is no number, and this read's outcome.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackendUsageView {
    pub backend_id: String,
    pub windows: Vec<UsageWindowView>,
    pub plan: Option<String>,
    /// Only when `windows` is empty: `unknown`, `not reported` or `no cap on
    /// this key`.
    pub note: Option<String>,
    pub read_at: Option<String>,
    pub read_error: Option<String>,
    /// The runtime's outcome for this read, e.g. `skipped_recent` or
    /// `unavailable: <reason>`; `None` when the runtime ran none for it.
    pub read: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageWindowView {
    pub label: String,
    pub window_minutes: Option<i64>,
    pub used_pct: f64,
    pub resets_at: Option<String>,
    /// `header`, `endpoint` or `error`.
    pub source: String,
    pub observed_at: String,
    /// Older than the staleness bound: the last known value.
    pub last_known: bool,
}

#[tauri::command]
pub(crate) async fn desktop_provider_account_rename<R: Runtime>(
    app: AppHandle<R>,
    request: ProviderAccountRenameRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let node_did = request.node_did.trim();
    let access = core
        .operator_access(node_did)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    gents::oauth_credential::set_account_label(
        &access,
        node_did,
        &request.credential_id,
        &request.label,
    )
    .await
    .map_err(|error| BridgeError::untyped(error.to_string()))?;
    notify_config_changed(&app, &core).await;
    Ok(())
}

/// Removes the account as `gents accounts remove --yes` does, with the
/// backends its sign-in created that no profile uses.
#[tauri::command]
pub(crate) async fn desktop_provider_account_remove<R: Runtime>(
    app: AppHandle<R>,
    request: ProviderAccountRemoveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let node_did = request.node_did.trim();
    let access = core
        .operator_access(node_did)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    gents_server::accounts::remove_account(&access, node_did, &request.credential_id, None, true)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    notify_config_changed(&app, &core).await;
    Ok(())
}

/// Each stored backend's usage as the panel draws it, with the runtime's
/// outcome when `reads` names it: by backend, else by its account.
async fn backend_usage_views(
    access: &gents::ConfigAccess,
    node_did: &str,
    reads: Option<&gents_server::accounts::UsageReads>,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<BackendUsageView>> {
    use gents::backend_provider::BackendProviderOauthExt as _;
    use gents::usage_observation::{account_usage::UsageSource, UsageRead};

    let backends = access
        .transact("desktop.usage_views", |txn| {
            Box::pin(async move {
                gents::config_client::list_inference_backends_in_txn(txn, node_did).await
            })
        })
        .await?;
    let reads = reads
        .filter(|reads| reads.node_did == node_did)
        .map_or(&[][..], |reads| reads.reads.as_slice());
    let time = |at: chrono::DateTime<chrono::Utc>| at.to_rfc3339();
    let mut views = Vec::with_capacity(backends.len());
    for backend in backends {
        let stored =
            gents::usage_observation::usage_for_backend(access, node_did, &backend).await?;
        let usage =
            gents::usage_observation::usage_view(stored.as_ref(), backend.provider_kind, now);
        let account = match (backend.provider_kind.oauth_provider(), &backend.auth) {
            (Some(provider), gents::document_config::BackendAuth::NodeOAuth { account_ref }) => {
                Some((provider, account_ref))
            }
            _ => None,
        };
        let read = reads.iter().find(|read| match &read.backend_id {
            Some(backend_id) => *backend_id == backend.backend_id,
            None => account.is_some_and(|(provider, account_ref)| {
                read.provider == provider && read.account_ref == *account_ref
            }),
        });
        views.push(BackendUsageView {
            backend_id: backend.backend_id,
            windows: usage
                .windows
                .into_iter()
                .map(|window| UsageWindowView {
                    label: window.label,
                    window_minutes: window.window_minutes,
                    used_pct: window.used_pct,
                    resets_at: window.resets_at.map(time),
                    source: match window.source {
                        UsageSource::Header => "header",
                        UsageSource::Endpoint => "endpoint",
                        UsageSource::Error => "error",
                    }
                    .to_string(),
                    observed_at: time(window.observed_at),
                    last_known: window.last_known,
                })
                .collect(),
            plan: usage.plan,
            note: usage.note.map(str::to_owned),
            read_at: usage.read_at.map(time),
            read_error: usage.read_error,
            read: read.map(|read| match &read.outcome {
                UsageRead::Read => "read".to_string(),
                UsageRead::SkippedRecent => "skipped_recent".to_string(),
                UsageRead::NotReported => "not_reported".to_string(),
                UsageRead::Disabled => "disabled".to_string(),
                UsageRead::Unavailable(reason) => format!("unavailable: {reason}"),
            }),
        });
    }
    Ok(views)
}

/// Asks the hosted runtime to read usage (skipping accounts read in the last
/// few minutes), signed with its own identity as `gents accounts list` signs
/// it, then returns each backend's stored usage. A failed read on open still
/// returns what is stored; a failed Refresh is an error.
#[tauri::command]
pub(crate) async fn desktop_provider_usage_read(
    request: ProviderUsageReadRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<BackendUsageView>, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let node_did = request.node_did.trim();
    let access = core
        .operator_access(node_did)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let trigger = if request.refresh {
        gents::usage_observation::UsageTrigger::Refresh
    } else {
        gents::usage_observation::UsageTrigger::Open
    };
    let reads = async {
        let signer = core.operator_signer(node_did)?;
        let graphql = core
            .operator_graphql(node_did)
            .ok_or_else(|| anyhow::anyhow!("the node has no operator endpoint"))?;
        gents_server::accounts::request_usage_reads(
            signer.as_ref(),
            &graphql,
            trigger,
            request.provider.as_deref(),
        )
        .await
    }
    .await;
    let reads = match reads {
        Ok(reads) => Some(reads),
        Err(error) if !request.refresh => {
            tracing::warn!(
                target: LOG_TARGET,
                error = %format!("{error:#}"),
                "usage read on open failed; showing stored usage"
            );
            None
        }
        Err(error) => return Err(BridgeError::untyped(error.to_string())),
    };
    backend_usage_views(&access, node_did, reads.as_ref(), chrono::Utc::now())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderAccountRetrySaveRequest {
    pub node_did: String,
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
    let node_did = request.node_did.trim();
    let provider = request.provider.trim();
    let credential = retry_pending_credential(
        &state.pending_oauth_credentials,
        core.operator_access(node_did),
        node_did,
        provider,
    )
    .await?;
    notify_config_changed(&app, &core).await;
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
    let node_did = request.node_did.trim().to_string();
    if node_did.is_empty() {
        return Err(BridgeError::untyped("node_did is required"));
    }
    let label = sign_in_label(request.label.as_deref())?;
    let provider = normalize_xai_provider(request.provider.as_deref().unwrap_or_default());
    require_reachable_configuration(core.operator_access(&node_did), &node_did, &provider).await?;

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

    let credential = OAuthCredential {
        label,
        ..credential_from_login_tokens(&node_did, &provider, &tokens, chrono::Utc::now())
    };
    let signed = save_issued_credential(
        &state.pending_oauth_credentials,
        core.operator_access(&node_did),
        state.pending_oauth_credentials.issue(credential.clone()),
    )
    .await?;

    notify_config_changed(&app, &core).await;

    Ok(GrokLoginResult::redacted(&signed))
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
            node_did: "did:key:zAgent".to_string(),
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
            account_ref: None,
            connected_at: None,
            provider_account_key: None,
            label: None,
        };
        let json = serde_json::to_string(&ProviderAccountView::from(&credential)).unwrap();
        assert!(!json.contains("secret-access"));
        assert!(!json.contains("secret-refresh"));
        assert!(!json.contains("secret-id"));
        assert!(json.contains("acct-1"));
    }

    fn issued_credential(node_did: &str) -> OAuthCredential {
        OAuthCredential {
            doc_id: None,
            credential_id: format!("claude-subscription:{node_did}"),
            node_did: node_did.to_string(),
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
            account_ref: None,
            connected_at: None,
            provider_account_key: None,
            label: None,
        }
    }

    /// Operator GraphQL access for a runtime that is not serving.
    fn unreachable_operator() -> gents::ConfigAccess {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
        let address = listener.local_addr().expect("local address");
        drop(listener);
        gents::ConfigAccess::graphql(format!("http://{address}/api/v0/graphql"))
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
                "managed node did:key:zAgent has no operator GraphQL endpoint"
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

    fn held_tokens(pending: &PendingOAuthCredentials, node_did: &str) -> Vec<String> {
        pending
            .held_for(node_did)
            .into_iter()
            .map(|credential| credential.access_token)
            .collect()
    }

    fn credential_with_token(node_did: &str, token: &str) -> OAuthCredential {
        OAuthCredential {
            access_token: token.to_string(),
            ..issued_credential(node_did)
        }
    }

    #[tokio::test]
    async fn a_desktop_sign_in_writes_its_own_key() {
        let node_did = "did:key:zAgent";
        let provider = gents::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        let node = serving_node().await;
        let keyed = OAuthCredential {
            credential_id: format!("{provider}:{node_did}"),
            provider: provider.to_string(),
            provider_account_key: Some("user:key-1".to_string()),
            // Grok rows carry no `account_id`.
            account_id: None,
            label: None,
            ..issued_credential(node_did)
        };
        gents::oauth_credential::upsert_oauth_credential_on(
            &gents::ConfigAccess::Local(node.clone()),
            &keyed,
        )
        .await
        .expect("seed a keyed row");

        let keyless = gents::xai_oauth_login::credential_from_login_tokens(
            node_did,
            provider,
            &gents::xai_oauth_login::XaiLoginTokens {
                access_token: "not-a-jwt".to_string(),
                refresh_token: "keyless-refresh".to_string(),
                id_token: None,
                expires_in: Some(900),
            },
            chrono::Utc::now(),
        );
        let pending = PendingOAuthCredentials::default();
        save_issued_credential(
            &pending,
            Ok(gents::ConfigAccess::Local(node.clone())),
            pending.issue(keyless),
        )
        .await
        .expect("save the sign-in");

        let stored = list_oauth_credentials_on(&gents::ConfigAccess::Local(node), node_did)
            .await
            .expect("list stored credentials");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].refresh_token, "keyless-refresh");
        assert_eq!(stored[0].provider_account_key, None);
    }

    #[tokio::test]
    async fn failed_save_holds_the_issued_credential_and_retry_saves_it_without_login() {
        let pending = PendingOAuthCredentials::default();
        let node_did = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let error = save_issued_credential(
            &pending,
            Ok(unreachable_operator()),
            pending.issue(issued_credential(node_did)),
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
        assert_eq!(held_tokens(&pending, node_did), ["issued-access"]);

        // A retry while the runtime is still down keeps the credential held.
        let still_down = retry_pending_credential(
            &pending,
            Err(anyhow::anyhow!("no operator GraphQL endpoint")),
            node_did,
            provider,
        )
        .await
        .expect_err("retry against an unavailable runtime fails");
        assert_eq!(still_down.code, BridgeErrorCode::CredentialNotSaved);
        assert_user_facing(&still_down);
        assert_eq!(held_tokens(&pending, node_did), ["issued-access"]);

        // Once the canonical owner serves, retry stores the same tokens.
        let node = serving_node().await;
        let saved = retry_pending_credential(
            &pending,
            Ok(gents::ConfigAccess::Local(node.clone())),
            node_did,
            provider,
        )
        .await
        .expect("retry stores the held credential");
        assert_eq!(saved.access_token, "issued-access");
        assert!(held_tokens(&pending, node_did).is_empty());
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::Local(node), node_did)
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
        let node_did = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let unobserved = observe_provider_accounts(&pending, Ok(unreachable_operator()), node_did)
            .await
            .expect_err("an unreadable store with nothing held stays an error");
        assert_eq!(unobserved.code, BridgeErrorCode::Unknown);

        let _ = save_issued_credential(
            &pending,
            Ok(unreachable_operator()),
            pending.issue(issued_credential(node_did)),
        )
        .await;
        let held = observe_provider_accounts(&pending, Ok(unreachable_operator()), node_did)
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
            node_did,
        )
        .await
        .expect("serving store");
        assert_eq!(with_store.len(), 1);
        assert!(with_store[0].pending_save);

        retry_pending_credential(
            &pending,
            Ok(gents::ConfigAccess::Local(node.clone())),
            node_did,
            provider,
        )
        .await
        .expect("retry stores the held credential");
        let saved =
            observe_provider_accounts(&pending, Ok(gents::ConfigAccess::Local(node)), node_did)
                .await
                .expect("serving store");
        assert_eq!(saved.len(), 1);
        assert!(!saved[0].pending_save);
        assert_eq!(saved[0].provider, provider);
    }

    #[tokio::test]
    async fn retry_save_only_uses_a_credential_held_for_that_node_and_provider() {
        let pending = PendingOAuthCredentials::default();
        let _ = save_issued_credential(
            &pending,
            Ok(unreachable_operator()),
            pending.issue(issued_credential("did:key:zAgent")),
        )
        .await;

        for (node_did, provider) in [
            ("did:key:zOther", gents::claude_oauth::CLAUDE_OAUTH_PROVIDER),
            (
                "did:key:zAgent",
                gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            ),
        ] {
            let error = retry_pending_credential(
                &pending,
                Ok(serving_operator().await),
                node_did,
                provider,
            )
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
        let node_did = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let older = pending.issue(credential_with_token(node_did, "token-a"));
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
                    .retry(node_did, provider, move |credential| async move {
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

        let newer = pending.issue(credential_with_token(node_did, "token-b"));
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
        assert_eq!(held_tokens(&pending, node_did), ["token-b"]);
        assert_eq!(*writes.lock().unwrap(), ["token-a"]);

        let retried = pending
            .retry(node_did, provider, {
                let writes = writes.clone();
                move |credential| async move {
                    writes.lock().unwrap().push(credential.access_token);
                    anyhow::Ok(())
                }
            })
            .await
            .map(|(credential, saved)| (credential.access_token, saved.is_ok()));
        assert_eq!(retried, Some(("token-b".to_string(), true)));
        assert!(held_tokens(&pending, node_did).is_empty());
        assert_eq!(*writes.lock().unwrap(), ["token-a", "token-b"]);
    }

    #[tokio::test]
    async fn an_older_sign_in_saved_after_a_newer_one_never_overwrites_it() {
        let pending = std::sync::Arc::new(PendingOAuthCredentials::default());
        let writes = Writes::default();
        let node_did = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;

        let older = pending.issue(credential_with_token(node_did, "token-a"));
        let newer = pending.issue(credential_with_token(node_did, "token-b"));

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

        let failed_older = pending.issue(credential_with_token(node_did, "token-c"));
        let saved_newer = pending.issue(credential_with_token(node_did, "token-d"));
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
        assert!(held_tokens(&pending, node_did).is_empty());
        assert!(pending
            .retry(node_did, provider, failed_write)
            .await
            .is_none());
        assert_eq!(*writes.lock().unwrap(), ["token-b", "token-d"]);
    }

    /// A Claude sign-in of `account` (key `org-1:{account}`, or none) whose
    /// display label is `account_id`.
    fn claude_sign_in(
        node_did: &str,
        account: Option<&str>,
        account_id: &str,
        token: &str,
    ) -> OAuthCredential {
        gents::claude_oauth::credential_from_login_tokens(
            node_did,
            gents::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            &gents::claude_oauth::ClaudeLoginTokens {
                access_token: token.into(),
                refresh_token: format!("refresh-{token}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(account_id.into()),
                organization_uuid: account.map(|_| "org-1".into()),
                account_uuid: account.map(str::to_owned),
            },
            chrono::Utc::now(),
        )
    }

    fn two_accounts(node_did: &str, keyed: bool) -> (OAuthCredential, OAuthCredential) {
        (
            claude_sign_in(node_did, keyed.then_some("account-a"), "acct-a", "token-a"),
            claude_sign_in(node_did, keyed.then_some("account-b"), "acct-b", "token-b"),
        )
    }

    fn saved_write(_: OAuthCredential) -> std::future::Ready<anyhow::Result<()>> {
        std::future::ready(Ok(()))
    }

    #[tokio::test]
    async fn a_held_sign_in_of_one_account_survives_a_saved_sign_in_of_another() {
        for keyed in [true, false] {
            let pending = PendingOAuthCredentials::default();
            let node_did = "did:key:zAgent";
            let (a, b) = two_accounts(node_did, keyed);
            assert!(matches!(
                pending.save(pending.issue(a), failed_write).await,
                CredentialSave::Failed(_)
            ));
            assert!(matches!(
                pending.save(pending.issue(b), saved_write).await,
                CredentialSave::Saved(())
            ));
            assert_eq!(
                held_tokens(&pending, node_did),
                ["token-a"],
                "keyed: {keyed}"
            );
        }
    }

    #[tokio::test]
    async fn an_older_sign_in_of_one_account_is_not_superseded_by_another() {
        for keyed in [true, false] {
            let pending = PendingOAuthCredentials::default();
            let node_did = "did:key:zAgent";
            let (a, b) = two_accounts(node_did, keyed);
            let older = pending.issue(a);
            let newer = pending.issue(b);
            assert!(matches!(
                pending.save(newer, saved_write).await,
                CredentialSave::Saved(())
            ));
            assert!(
                matches!(
                    pending.save(older, saved_write).await,
                    CredentialSave::Saved(())
                ),
                "keyed: {keyed}"
            );
        }
        let pending = PendingOAuthCredentials::default();
        let node_did = "did:key:zAgent";
        let older = pending.issue(claude_sign_in(
            node_did,
            Some("account-a"),
            "acct-a",
            "token-1",
        ));
        let newer = pending.issue(claude_sign_in(
            node_did,
            Some("account-a"),
            "acct-a",
            "token-2",
        ));
        assert!(matches!(
            pending.save(newer, saved_write).await,
            CredentialSave::Saved(())
        ));
        assert!(matches!(
            pending.save(older, saved_write).await,
            CredentialSave::Superseded
        ));
    }

    #[tokio::test]
    async fn a_second_accounts_desktop_sign_in_is_added_beside_the_first() {
        let node = serving_node().await;
        let access = || Ok(gents::ConfigAccess::Local(node.clone()));
        let node_did = "did:key:zAgent";
        let pending = PendingOAuthCredentials::default();
        let (a, b) = two_accounts(node_did, true);
        save_issued_credential(&pending, access(), pending.issue(a))
            .await
            .expect("save the first account");
        let first = list_oauth_credentials_on(&gents::ConfigAccess::Local(node.clone()), node_did)
            .await
            .unwrap();
        let signed = save_issued_credential(&pending, access(), pending.issue(b))
            .await
            .expect("save the second account");
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::Local(node.clone()), node_did)
            .await
            .unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.contains(&first[0]), "the first account is untouched");
        let second = stored
            .iter()
            .find(|row| row.doc_id != first[0].doc_id)
            .unwrap();
        assert_eq!(signed.credential.credential_id, second.credential_id);
        assert_ne!(signed.credential.credential_id, first[0].credential_id);
        let views = observe_provider_accounts(&pending, access(), node_did)
            .await
            .unwrap();
        let view = |credential_id: &str| {
            views
                .iter()
                .find(|view| view.credential_id == credential_id)
                .unwrap()
                .account_ref
                .clone()
        };
        assert_eq!(view(&first[0].credential_id), None);
        assert!(second.account_ref.is_some());
        assert_eq!(view(&second.credential_id), second.account_ref);
    }

    /// The added account's backend is written on the runtime through its
    /// operator GraphQL, which never replicates to the desktop node, so the
    /// row shows only once the client store is refreshed from the runtime.
    #[tokio::test]
    async fn an_added_accounts_backend_is_in_the_client_snapshot_after_sign_in() {
        use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};

        let temp = tempfile::TempDir::new().expect("tmpdir");
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("free port");
        let runtime = gents::defra_node::EmbeddedNode::builder()
            .with_http(gents::defra_node::HttpConfig::with_addr(address))
            .build()
            .await
            .expect("runtime node");
        gents::ensure_runtime_schemas(&runtime)
            .await
            .expect("schemas");
        let graphql = format!("http://{address}/api/v0/graphql");
        let options = || gents_protocol::graphql::GraphqlRequestOptions {
            timeout: Duration::from_secs(1),
            max_attempts: 1,
            retry_backoff: Duration::ZERO,
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            while !gents_protocol::graphql::graphql_endpoint_available(&graphql, options()).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("runtime GraphQL listener");

        let node_did = "did:key:zAgent";
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(temp.path().join("client")),
            ClientCoreOptions::local_only(),
        )
        .await
        .expect("client core");
        let home = temp.path().join("node");
        core.add_managed_enrollment_peer_for_test(node_did, &graphql, &home.to_string_lossy(), 1)
            .await
            .expect("runtime record");
        core.set_selected_node_did(Some(node_did.to_string()));
        assert!(matches!(
            core.operator_access(node_did),
            Ok(gents::ConfigAccess::Graphql(_))
        ));

        let pending = PendingOAuthCredentials::default();
        let (a, b) = two_accounts(node_did, true);
        for account in [a, b] {
            save_issued_credential(
                &pending,
                core.operator_access(node_did),
                pending.issue(account),
            )
            .await
            .expect("save the account");
        }
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        notify_config_changed(app.handle(), &core).await;

        let snapshot = core.store().snapshot();
        let backends: Vec<_> = snapshot
            .inference_backends
            .iter()
            .filter(|backend| backend.node_did == node_did)
            .map(|backend| backend.backend_id.as_str())
            .collect();
        assert!(
            backends
                .iter()
                .any(|id| id.starts_with("claude-subscription-")),
            "the added account's backend is in the client snapshot: {backends:?}"
        );
        core.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn accounts_list_in_resolver_order() {
        let node = serving_node().await;
        let access = gents::ConfigAccess::Local(node.clone());
        let node_did = "did:key:zOrder";
        let at = |secs| chrono::DateTime::from_timestamp(secs, 0);
        // Stored so that insertion order is not resolver order.
        for (id, connected_at, enabled) in [
            ("a", at(2_000), true),
            ("b", at(1_000), true),
            ("c", None, false),
        ] {
            let credential = OAuthCredential {
                credential_id: format!("claude-subscription:{node_did}:{id}"),
                account_ref: Some(id.to_string()),
                connected_at,
                enabled,
                ..issued_credential(node_did)
            };
            gents::oauth_credential::upsert_oauth_credential_on(&access, &credential)
                .await
                .expect("store account");
        }

        let accounts = observe_provider_accounts(
            &PendingOAuthCredentials::default(),
            Ok(gents::ConfigAccess::Local(node)),
            node_did,
        )
        .await
        .expect("observe accounts");
        let ids: Vec<&str> = accounts
            .iter()
            .map(|account| account.credential_id.rsplit(':').next().unwrap())
            .collect();
        assert_eq!(ids, ["c", "b", "a"]);

        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;
        let first_match = accounts
            .iter()
            .find(|account| {
                account.provider == provider && account.enabled && !account.pending_save
            })
            .map(|account| account.credential_id.clone());
        let resolved = gents::oauth_credential::resolve_oauth_credential(
            &access,
            node_did,
            provider,
            gents::oauth_credential::AccountPick::ProviderDefault,
        )
        .await
        .expect("resolve")
        .map(|credential| credential.credential_id);
        assert_eq!(first_match, resolved);
    }

    #[tokio::test]
    async fn the_account_view_carries_the_label_never_a_token_or_key() {
        let access = serving_operator().await;
        let node_did = "did:key:zAgent";
        gents::oauth_credential::store_sign_in(
            &access,
            claude_sign_in(node_did, None, "acct-a", "SECRET-a"),
            None,
        )
        .await
        .expect("store the first account");
        gents::oauth_credential::store_sign_in(
            &access,
            OAuthCredential {
                provider_account_key: Some("KEY-SENTINEL".to_string()),
                ..claude_sign_in(node_did, None, "acct-b", "SECRET-b")
            },
            Some("Work"),
        )
        .await
        .expect("store the labelled account");

        let views =
            observe_provider_accounts(&PendingOAuthCredentials::default(), Ok(access), node_did)
                .await
                .expect("observe accounts");
        let labels: Vec<&str> = views.iter().map(|view| view.label.as_str()).collect();
        assert_eq!(labels, ["Claude", "Work"]);
        let json = serde_json::to_string(&views).unwrap();
        assert!(!json.contains("SECRET"));
        assert!(!json.contains("KEY-SENTINEL"));
    }

    #[tokio::test]
    async fn a_labelled_sign_in_reports_its_result_and_keeps_its_label_on_retry() {
        let node = serving_node().await;
        let access = || Ok(gents::ConfigAccess::Local(node.clone()));
        let node_did = "did:key:zAgent";
        let pending = PendingOAuthCredentials::default();
        let labelled = |label: &str, account: &str, token: &str| OAuthCredential {
            label: Some(label.to_string()),
            ..claude_sign_in(node_did, Some(account), account, token)
        };
        save_issued_credential(
            &pending,
            access(),
            pending.issue(claude_sign_in(
                node_did,
                Some("acct-a"),
                "acct-a",
                "SECRET-a",
            )),
        )
        .await
        .expect("save the first account");

        let added = SignInView::from(
            &save_issued_credential(
                &pending,
                access(),
                pending.issue(labelled("Work", "acct-b", "SECRET-b")),
            )
            .await
            .expect("add the labelled account"),
        );
        assert_eq!(added.result, "added");
        assert_eq!(added.label, "Work");
        assert!(added.account_ref.is_some());
        assert_eq!(added.hint, None);

        let refreshed = SignInView::from(
            &save_issued_credential(
                &pending,
                access(),
                pending.issue(claude_sign_in(
                    node_did,
                    Some("acct-b"),
                    "acct-b",
                    "SECRET-c",
                )),
            )
            .await
            .expect("refresh the labelled account"),
        );
        assert_eq!(refreshed.result, "refreshed");
        assert_eq!(refreshed.label, "Work");
        assert_eq!(refreshed.account_ref, added.account_ref);
        assert!(refreshed
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("Work")));

        save_issued_credential(
            &pending,
            Ok(unreachable_operator()),
            pending.issue(labelled("Spare", "acct-d", "SECRET-d")),
        )
        .await
        .expect_err("a runtime that is not serving cannot store the sign-in");
        retry_pending_credential(
            &pending,
            access(),
            node_did,
            gents::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        )
        .await
        .expect("retry stores the held sign-in");
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::Local(node.clone()), node_did)
            .await
            .unwrap();
        let spare = stored
            .iter()
            .find(|row| row.account_id.as_deref() == Some("acct-d"))
            .expect("the retried account is stored");
        assert_eq!(spare.label.as_deref(), Some("Spare"));

        let json = serde_json::to_string(&[&added, &refreshed]).unwrap();
        assert!(!json.contains("SECRET"));
    }

    #[tokio::test]
    async fn a_label_names_only_an_added_account_never_a_refreshed_one() {
        let node = serving_node().await;
        let access = || Ok(gents::ConfigAccess::Local(node.clone()));
        let node_did = "did:key:zAgent";
        let pending = PendingOAuthCredentials::default();
        let labelled = |label: Option<&str>, account: &str, token: &str| OAuthCredential {
            label: label.map(str::to_string),
            ..claude_sign_in(node_did, Some(account), account, token)
        };
        for (label, account) in [(None, "acct-a"), (Some("Work"), "acct-b")] {
            save_issued_credential(
                &pending,
                access(),
                pending.issue(labelled(label, account, "SECRET-a")),
            )
            .await
            .expect("store the account");
        }

        for (account, kept) in [("acct-b", "Work"), ("acct-a", "Claude")] {
            let refreshed = SignInView::from(
                &save_issued_credential(
                    &pending,
                    access(),
                    pending.issue(labelled(Some("Spare"), account, "SECRET-c")),
                )
                .await
                .expect("refresh the stored account"),
            );
            assert_eq!(refreshed.result, "refreshed");
            assert_eq!(refreshed.label, kept);
        }
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::Local(node.clone()), node_did)
            .await
            .unwrap();
        let labels: Vec<String> = stored
            .iter()
            .map(gents::oauth_credential::effective_account_label)
            .collect();
        assert_eq!(labels, ["Claude", "Work"]);
    }

    #[tokio::test]
    async fn an_added_account_whose_label_is_taken_is_saved_under_its_default_label() {
        let node = serving_node().await;
        let access = || Ok(gents::ConfigAccess::Local(node.clone()));
        let node_did = "did:key:zAgent";
        let pending = PendingOAuthCredentials::default();
        let labelled = |label: Option<&str>, account: &str| OAuthCredential {
            label: label.map(str::to_string),
            ..claude_sign_in(node_did, Some(account), account, "SECRET")
        };
        for (label, account) in [(None, "acct-a"), (Some("Work"), "acct-b")] {
            save_issued_credential(&pending, access(), pending.issue(labelled(label, account)))
                .await
                .expect("store the account");
        }

        let saved = save_issued_credential(
            &pending,
            access(),
            pending.issue(labelled(Some("Work"), "acct-c")),
        )
        .await;
        assert!(
            saved.is_ok(),
            "a stored account is never reported unsaved: {:?}",
            saved.as_ref().err()
        );
        let view = SignInView::from(&saved.unwrap());
        assert_eq!(view.result, "added");
        assert_eq!(view.label, "Claude 2");
    }

    #[test]
    fn a_sign_in_label_is_trimmed_and_checked_before_the_browser() {
        assert_eq!(
            sign_in_label(Some("  Work ")).unwrap().as_deref(),
            Some("Work")
        );
        assert_eq!(sign_in_label(Some("   ")).unwrap(), None);
        assert_eq!(sign_in_label(None).unwrap(), None);
        for refused in ["x".repeat(65), "Wo\u{7}rk".to_string()] {
            let error = sign_in_label(Some(&refused)).expect_err("an invalid label is refused");
            assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
        }
    }

    /// Serves `body` with 200 to one request; returns the server's origin.
    fn serve_once(body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
        let address = listener.local_addr().expect("local address");
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.read(&mut [0u8; 4096]);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        format!("http://{address}")
    }

    fn stored_backend(node_did: &str, backend: serde_json::Value) -> gents::InferenceBackend {
        let mut backend = backend;
        backend["node_did"] = node_did.into();
        serde_json::from_value(backend).expect("backend")
    }

    #[tokio::test]
    async fn usage_views_follow_each_backend_with_source_age_and_honest_unknowns() {
        use gents::usage_observation::account_usage::{UsageReport, UsageSource, UsageWindow};
        use gents::usage_observation::{
            read_account_usage, record_usage, AccountUsageRead, UsageAccount, UsageEndpoints,
            UsageRead, UsageTrigger,
        };

        let node = serving_node().await;
        let access = gents::ConfigAccess::Local(node.clone());
        let node_did = "did:key:zAgent";
        let now = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp(), 0).unwrap();
        let minutes = chrono::Duration::minutes;
        let window = |label: &str, used_pct, source, observed_at, resets_at| UsageWindow {
            label: label.to_string(),
            window_minutes: None,
            used_pct,
            resets_at: Some(resets_at),
            source,
            observed_at,
        };
        let report = |windows| UsageReport {
            windows,
            ..UsageReport::default()
        };

        let a = gents::oauth_credential::store_sign_in(
            &access,
            claude_sign_in(node_did, Some("account-a"), "acct-a", "SECRET-a"),
            None,
        )
        .await
        .expect("store the original account")
        .credential;
        let b = gents::oauth_credential::store_sign_in(
            &access,
            OAuthCredential {
                provider_account_key: Some("KEY-SENTINEL".to_string()),
                ..claude_sign_in(node_did, Some("account-b"), "IDENTITY", "SECRET-b")
            },
            Some("Work"),
        )
        .await
        .expect("store the added account")
        .credential;
        let b_backend = format!("claude-subscription-{}", b.account_ref.clone().unwrap());
        let openrouter_origin = serve_once(r#"{"data":{"limit":null}}"#);
        let openrouter = stored_backend(
            node_did,
            serde_json::json!({
                "backend_id": "openrouter",
                "name": "OpenRouter",
                "provider_kind": "OpenRouter",
                "endpoint": format!("{openrouter_origin}/api/v1"),
                "auth": { "kind": "api_key", "key": "SECRET-or" },
            }),
        );
        for backend in [
            stored_backend(
                node_did,
                serde_json::json!({
                    "backend_id": "claude",
                    "name": "Claude",
                    "provider_kind": "ClaudeCliSubscription",
                    "endpoint": "https://api.anthropic.com",
                    "auth": { "kind": "node_oauth" },
                }),
            ),
            openrouter.clone(),
            stored_backend(
                node_did,
                serde_json::json!({
                    "backend_id": "local",
                    "name": "Local",
                    "provider_kind": "OpenAiCompatible",
                    "endpoint": "http://127.0.0.1:9/v1",
                    "auth": { "kind": "unauthenticated" },
                }),
            ),
        ] {
            gents::config_client::write_inference_backend_document(&access, &backend)
                .await
                .expect("store backend");
        }

        record_usage(
            &node,
            &UsageAccount::for_credential(&b),
            report(vec![
                window(
                    "5h",
                    40.0,
                    UsageSource::Header,
                    now - minutes(3),
                    now + minutes(133),
                ),
                window(
                    "7d",
                    90.0,
                    UsageSource::Header,
                    now - minutes(70),
                    now + minutes(600),
                ),
                window(
                    "1d",
                    55.0,
                    UsageSource::Header,
                    now - minutes(20),
                    now + minutes(300),
                ),
            ]),
        )
        .await
        .expect("record B");
        record_usage(
            &node,
            &UsageAccount::for_credential(&a),
            report(vec![
                window(
                    "5h",
                    70.0,
                    UsageSource::Header,
                    now - minutes(5),
                    now - minutes(1),
                ),
                window(
                    "7d",
                    20.0,
                    UsageSource::Endpoint,
                    now - minutes(2),
                    now + minutes(600),
                ),
            ]),
        )
        .await
        .expect("record A");
        assert_eq!(
            read_account_usage(
                node.clone(),
                node_did,
                &openrouter,
                UsageTrigger::Refresh,
                &UsageEndpoints::default(),
                now,
            )
            .await
            .expect("read OpenRouter"),
            UsageRead::Read
        );

        let reads = gents_server::accounts::UsageReads {
            node_did: node_did.to_string(),
            reads: vec![
                AccountUsageRead {
                    provider: gents::claude_oauth::CLAUDE_OAUTH_PROVIDER.to_string(),
                    account_ref: b.account_ref.clone(),
                    backend_id: None,
                    outcome: UsageRead::SkippedRecent,
                },
                AccountUsageRead {
                    provider: "OpenRouter".to_string(),
                    account_ref: None,
                    backend_id: Some("openrouter".to_string()),
                    outcome: UsageRead::Unavailable("throttled".to_string()),
                },
            ],
        };
        let views = backend_usage_views(&access, node_did, Some(&reads), now)
            .await
            .expect("usage views");
        let view = |backend_id: &str| {
            views
                .iter()
                .find(|view| view.backend_id == backend_id)
                .unwrap_or_else(|| panic!("no usage view for {backend_id}: {views:?}"))
        };

        let work = view(&b_backend);
        assert_eq!(work.windows.len(), 2, "{work:?}");
        let window = |label: &str| {
            work.windows
                .iter()
                .find(|window| window.label == label)
                .unwrap_or_else(|| panic!("no {label} window: {work:?}"))
        };
        let five_hours = window("5h");
        assert_eq!(five_hours.label, "5h");
        assert_eq!(five_hours.used_pct, 40.0);
        assert_eq!(five_hours.source, "header");
        assert_eq!(five_hours.observed_at, (now - minutes(3)).to_rfc3339());
        assert_eq!(
            five_hours.resets_at.as_deref(),
            Some((now + minutes(133)).to_rfc3339().as_str())
        );
        assert!(!five_hours.last_known);
        assert!(
            window("1d").last_known,
            "a window seen 20 minutes ago is last known"
        );
        assert_eq!(work.note, None);
        assert_eq!(work.read.as_deref(), Some("skipped_recent"));

        let original = view("claude");
        assert_eq!(original.windows.len(), 1, "{original:?}");
        assert_eq!(original.windows[0].label, "7d");
        assert_eq!(original.windows[0].used_pct, 20.0);
        assert_eq!(original.windows[0].source, "endpoint");
        assert_eq!(original.note, None);
        assert_eq!(original.read, None);

        let key = view("openrouter");
        assert!(key.windows.is_empty(), "{key:?}");
        assert_eq!(key.note.as_deref(), Some("no cap on this key"));
        assert!(key.read_at.is_some());
        assert_eq!(key.read.as_deref(), Some("unavailable: throttled"));

        let local = view("local");
        assert!(local.windows.is_empty());
        assert_eq!(local.note.as_deref(), Some("not reported"));
        assert_eq!(local.read, None);

        let json = serde_json::to_string(&views).unwrap();
        for secret in ["SECRET", "KEY-SENTINEL", "IDENTITY"] {
            assert!(!json.contains(secret), "usage views leak {secret}: {json}");
        }
    }

    #[tokio::test]
    async fn discovery_reads_the_account_its_backend_names() {
        use gents::document_config::BackendAuth;

        let access = serving_operator().await;
        let node_did = "did:key:zAgent";
        let provider = gents::claude_oauth::CLAUDE_OAUTH_PROVIDER;
        let a = gents::oauth_credential::store_sign_in(
            &access,
            claude_sign_in(node_did, Some("account-a"), "acct-a", "SECRET-a"),
            None,
        )
        .await
        .expect("store the original account")
        .credential;
        let b = gents::oauth_credential::store_sign_in(
            &access,
            claude_sign_in(node_did, Some("account-b"), "acct-b", "SECRET-b"),
            Some("Work"),
        )
        .await
        .expect("store the added account")
        .credential;
        assert!(b.account_ref.is_some());

        let (credential, auth) =
            discovery_account(&access, node_did, provider, b.account_ref.as_deref())
                .await
                .expect("added account");
        assert_eq!(
            credential.map(|row| row.credential_id),
            Some(b.credential_id.clone())
        );
        assert_eq!(
            auth,
            BackendAuth::NodeOAuth {
                account_ref: b.account_ref.clone()
            }
        );

        let (credential, auth) = discovery_account(&access, node_did, provider, None)
            .await
            .expect("original account");
        assert_eq!(
            credential.map(|row| row.credential_id),
            Some(a.credential_id)
        );
        assert_eq!(auth, BackendAuth::NodeOAuth { account_ref: None });

        gents::oauth_credential::set_account_enabled(&access, node_did, &b.credential_id, false)
            .await
            .expect("disable the added account");
        let (credential, _) =
            discovery_account(&access, node_did, provider, b.account_ref.as_deref())
                .await
                .expect("disabled account");
        assert_eq!(credential, None);
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
    pub node_did: String,
    #[serde(default)]
    pub provider: Option<String>,
    /// The new account's label; empty or absent leaves the store's default.
    #[serde(default)]
    pub label: Option<String>,
}

/// Redacted credential metadata for the webview (tokens never cross the bridge).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeLoginResult {
    pub doc_id: String,
    pub credential_id: String,
    pub node_did: String,
    pub provider: String,
    pub access_token_expires_at: String,
    pub enabled: bool,
    pub sign_in: SignInView,
}

impl ClaudeLoginResult {
    fn redacted(signed: &SignIn) -> Self {
        let credential = &signed.credential;
        Self {
            doc_id: signed.doc_id.clone(),
            credential_id: credential.credential_id.clone(),
            node_did: credential.node_did.clone(),
            provider: credential.provider.clone(),
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            enabled: credential.enabled,
            sign_in: SignInView::from(signed),
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
    let node_did = request.node_did.trim().to_string();
    if node_did.is_empty() {
        return Err(BridgeError::untyped("node_did is required"));
    }
    let label = sign_in_label(request.label.as_deref())?;
    let provider = normalize_provider(request.provider.as_deref().unwrap_or_default());
    require_reachable_configuration(core.operator_access(&node_did), &node_did, &provider).await?;

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
        organization_uuid: tokens.organization_uuid,
        account_uuid: tokens.account_uuid,
    };
    let credential = OAuthCredential {
        label,
        ..credential_from_login_tokens(&node_did, &provider, &login_tokens, chrono::Utc::now())
    };
    let signed = save_issued_credential(
        &state.pending_oauth_credentials,
        core.operator_access(&node_did),
        state.pending_oauth_credentials.issue(credential.clone()),
    )
    .await?;

    notify_config_changed(&app, &core).await;

    Ok(ClaudeLoginResult::redacted(&signed))
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
