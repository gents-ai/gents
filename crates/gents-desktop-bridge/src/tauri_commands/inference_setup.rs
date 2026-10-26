//! First-party inference onboarding commands for the desktop app. OAuth
//! credentials remain node-scoped documents and never cross this bridge
//! unredacted.

use crate::accounts::{observe_provider_accounts, retry_pending_credential};
#[cfg(test)]
use crate::accounts::{require_reachable_configuration, save_issued_credential};
use gents_protocol::error_chain::ErrorChain;
use std::time::Duration;

use gents::inference_setup::{
    InferenceAuthMethod, InferenceModelRecommendation, InferenceSetupCatalog,
};
#[cfg(test)]
use gents::oauth_credential::list_oauth_credentials_on;
#[cfg(test)]
use gents::oauth_credential::OAuthCredential;
use gents::oauth_credential::SignIn;
use gents_desktop_core::client::ClientCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime, State};

#[cfg(test)]
use crate::accounts::{CredentialSave, PendingOAuthCredentials};
use crate::error::{BridgeError, BridgeErrorCode};
#[cfg(test)]
use crate::inference_setup::discovery_account;
pub use crate::inference_setup::{
    discover_inference_models_for_core, inference_backend_recommendation,
    inference_model_recommendation, InferenceBackendRecommendationRequest,
    InferenceDiscoveryFailure, InferenceDiscoveryRequest, InferenceDiscoveryResult,
    InferenceRecommendationRequest,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::ClientUpdateEvent;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

const LOG_TARGET: &str = "gents_desktop_bridge::inference_setup";

mod sign_in;
mod sign_in_platform;

async fn notify_config_changed<R>(app: &AppHandle<R>, core: &ClientCore)
where
    R: Runtime,
{
    if let Err(error) = core.refresh_store().await {
        tracing::warn!(
            target: LOG_TARGET,
            error = %ErrorChain(&error),
            "refreshing the client store after a configuration write failed"
        );
    }
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
}

#[tauri::command]
pub(crate) fn desktop_inference_setup_catalog() -> InferenceSetupCatalog {
    sign_in_platform::setup_catalog(cfg!(target_os = "ios"))
}

#[tauri::command]
pub(crate) fn desktop_inference_model_recommendation(
    request: InferenceRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    inference_model_recommendation(request)
}

#[tauri::command]
pub(crate) fn desktop_inference_backend_recommendation(
    request: InferenceBackendRecommendationRequest,
) -> Result<InferenceModelRecommendation, BridgeError> {
    inference_backend_recommendation(request)
}

#[tauri::command]
pub(crate) async fn desktop_inference_models_discover(
    request: InferenceDiscoveryRequest,
    state: State<'_, DesktopAppState>,
) -> Result<InferenceDiscoveryResult, BridgeError> {
    let core = current_core(&state);
    discover_inference_models_for_core(request, core.as_deref()).await
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
    #[serde(default)]
    pub label: Option<String>,
}

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
pub(crate) async fn desktop_codex_login<R>(
    app: AppHandle<R>,
    window: tauri::WebviewWindow<R>,
    request: CodexLoginRequest,
    state: State<'_, DesktopAppState>,
) -> Result<CodexLoginResult, BridgeError>
where
    R: Runtime,
{
    let provider =
        gents::chatgpt_codex::normalize_provider(request.provider.as_deref().unwrap_or_default());
    let signed = sign_in::run(
        &app,
        &state,
        window.label(),
        request.node_did.trim(),
        InferenceAuthMethod::ChatGptOauth,
        &provider,
        request.label.as_deref(),
    )
    .await?;
    Ok(CodexLoginResult::redacted(&signed))
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexLoginUrl {
    pub url: String,
}

#[tauri::command]
pub(crate) fn desktop_codex_login_cancel<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    sign_in::cancel(&state, window.label(), InferenceAuthMethod::ChatGptOauth)
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrokLoginRequest {
    pub node_did: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

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

pub(crate) use crate::accounts::{
    ProviderAccountDisconnectRequest, ProviderAccountRemoveRequest, ProviderAccountRenameRequest,
    ProviderAccountRetrySaveRequest, ProviderAccountView, ProviderAccountsRequest, SignInView,
};

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
        core.operator_access(node_did).map_err(Into::into),
        node_did,
    )
    .await
}

#[tauri::command]
pub(crate) async fn desktop_provider_account_disconnect<R>(
    app: AppHandle<R>,
    request: ProviderAccountDisconnectRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
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
pub(crate) struct ProviderUsageReadRequest {
    pub node_did: String,
    pub refresh: bool,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackendUsageView {
    pub backend_id: String,
    pub windows: Vec<UsageWindowView>,
    pub plan: Option<String>,
    pub note: Option<String>,
    pub read_at: Option<String>,
    pub read_error: Option<String>,
    pub read: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageWindowView {
    pub label: String,
    pub window_minutes: Option<i64>,
    pub used_pct: f64,
    pub resets_at: Option<String>,
    pub source: String,
    pub observed_at: String,
    /// Older than the staleness bound: the last known value.
    pub last_known: bool,
}

#[tauri::command]
pub(crate) async fn desktop_provider_account_rename<R>(
    app: AppHandle<R>,
    request: ProviderAccountRenameRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
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

#[tauri::command]
pub(crate) async fn desktop_provider_account_remove<R>(
    app: AppHandle<R>,
    request: ProviderAccountRemoveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
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

#[derive(Debug, thiserror::Error)]
enum UsageReadsError {
    #[error("the node has no operator endpoint")]
    NoOperatorEndpoint,
    #[error(transparent)]
    Core(#[from] gents_desktop_core::client::ClientCoreError),
    #[error(transparent)]
    Accounts(#[from] gents_server::accounts::AccountsError),
}

#[derive(Debug, thiserror::Error)]
enum UsageViewsError {
    #[error(transparent)]
    InferenceBackend(#[from] gents::config_client::InferenceBackendError),
    #[error(transparent)]
    UsageObservation(#[from] gents::usage_observation::UsageObservationError),
}

async fn backend_usage_views(
    access: &gents::ConfigAccess,
    node_did: &str,
    reads: Option<&gents_server::accounts::UsageReads>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<BackendUsageView>, UsageViewsError> {
    use gents::backend_provider::BackendProviderOauthExt as _;
    use gents::usage_observation::{account_usage::UsageSource, UsageRead};

    let backends = access
        .transact_write("desktop.usage_views", |txn| {
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
            .ok_or(UsageReadsError::NoOperatorEndpoint)?;
        Ok::<_, UsageReadsError>(
            gents_server::accounts::request_usage_reads(
                signer.as_ref(),
                &graphql,
                trigger,
                request.provider.as_deref(),
            )
            .await?,
        )
    }
    .await;
    let reads = match reads {
        Ok(reads) => Some(reads),
        Err(error) if !request.refresh => {
            tracing::warn!(
                target: LOG_TARGET,
                error = %ErrorChain(&error),
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

#[tauri::command]
pub(crate) async fn desktop_provider_account_retry_save<R>(
    app: AppHandle<R>,
    request: ProviderAccountRetrySaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ProviderAccountView, BridgeError>
where
    R: Runtime,
{
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
        core.operator_access(node_did).map_err(Into::into),
        node_did,
        provider,
    )
    .await?;
    notify_config_changed(&app, &core).await;
    Ok(ProviderAccountView::from(&credential))
}

#[tauri::command]
pub(crate) async fn desktop_grok_login<R>(
    app: AppHandle<R>,
    window: tauri::WebviewWindow<R>,
    request: GrokLoginRequest,
    state: State<'_, DesktopAppState>,
) -> Result<GrokLoginResult, BridgeError>
where
    R: Runtime,
{
    let provider =
        gents::xai_grok_oauth::normalize_provider(request.provider.as_deref().unwrap_or_default());
    let signed = sign_in::run(
        &app,
        &state,
        window.label(),
        request.node_did.trim(),
        InferenceAuthMethod::GrokOauth,
        &provider,
        request.label.as_deref(),
    )
    .await?;
    Ok(GrokLoginResult::redacted(&signed))
}

#[cfg(test)]
mod provider_account_tests {
    use std::sync::Arc;

    use super::*;

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

    fn unreachable_operator() -> gents::ConfigAccess {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
        let address = listener.local_addr().expect("local address");
        drop(listener);
        gents::ConfigAccess::graphql(format!("http://{address}/api/v0/graphql"))
    }

    async fn serving_node() -> std::sync::Arc<gents::defra_node::EmbeddedNode> {
        let node = crate::tests::support::node_builder()
            .build()
            .await
            .expect("embedded node");
        gents::ensure_runtime_schemas(&node).await.expect("schemas");
        std::sync::Arc::new(node)
    }

    async fn serving_operator() -> gents::ConfigAccess {
        gents::ConfigAccess::local(serving_node().await)
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
            Err(crate::accounts::AccountsError::NotConnected),
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
            account_id: None,
            label: None,
            ..issued_credential(node_did)
        };
        gents::oauth_credential::upsert_oauth_credential_on(
            &gents::ConfigAccess::local(node.clone()),
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
            Ok(gents::ConfigAccess::local(node.clone())),
            pending.issue(keyless),
        )
        .await
        .expect("save the sign-in");

        let stored = list_oauth_credentials_on(&gents::ConfigAccess::local(node), node_did)
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

        let still_down = retry_pending_credential(
            &pending,
            Err(crate::accounts::AccountsError::NotConnected),
            node_did,
            provider,
        )
        .await
        .expect_err("retry against an unavailable runtime fails");
        assert_eq!(still_down.code, BridgeErrorCode::CredentialNotSaved);
        assert_user_facing(&still_down);
        assert_eq!(held_tokens(&pending, node_did), ["issued-access"]);

        let node = serving_node().await;
        let saved = retry_pending_credential(
            &pending,
            Ok(gents::ConfigAccess::local(node.clone())),
            node_did,
            provider,
        )
        .await
        .expect("retry stores the held credential");
        assert_eq!(saved.access_token, "issued-access");
        assert!(held_tokens(&pending, node_did).is_empty());
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::local(node), node_did)
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
            Ok(gents::ConfigAccess::local(node.clone())),
            node_did,
        )
        .await
        .expect("serving store");
        assert_eq!(with_store.len(), 1);
        assert!(with_store[0].pending_save);

        retry_pending_credential(
            &pending,
            Ok(gents::ConfigAccess::local(node.clone())),
            node_did,
            provider,
        )
        .await
        .expect("retry stores the held credential");
        let saved =
            observe_provider_accounts(&pending, Ok(gents::ConfigAccess::local(node)), node_did)
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

    fn failed_write(
        _: OAuthCredential,
    ) -> std::future::Ready<Result<(), crate::accounts::AccountsError>> {
        std::future::ready(Err(crate::accounts::AccountsError::NotConnected))
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
                        Ok::<_, crate::accounts::AccountsError>(())
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
                    Ok::<_, crate::accounts::AccountsError>(())
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
                            Ok::<_, crate::accounts::AccountsError>(())
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
                            Ok::<_, crate::accounts::AccountsError>(())
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
                        Ok::<_, crate::accounts::AccountsError>(())
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

    fn saved_write(
        _: OAuthCredential,
    ) -> std::future::Ready<Result<(), crate::accounts::AccountsError>> {
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
        let access = || Ok(gents::ConfigAccess::local(node.clone()));
        let node_did = "did:key:zAgent";
        let pending = PendingOAuthCredentials::default();
        let (a, b) = two_accounts(node_did, true);
        save_issued_credential(&pending, access(), pending.issue(a))
            .await
            .expect("save the first account");
        let first = list_oauth_credentials_on(&gents::ConfigAccess::local(node.clone()), node_did)
            .await
            .unwrap();
        let signed = save_issued_credential(&pending, access(), pending.issue(b))
            .await
            .expect("save the second account");
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::local(node.clone()), node_did)
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

    #[tokio::test]
    async fn an_added_accounts_backend_is_in_the_client_snapshot_after_sign_in() {
        use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};

        let temp = tempfile::TempDir::new().expect("tmpdir");
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("free port");
        let runtime = crate::tests::support::node_builder()
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
                core.operator_access(node_did).map_err(Into::into),
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
        Arc::new(core).shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn accounts_list_in_resolver_order() {
        let node = serving_node().await;
        let access = gents::ConfigAccess::local(node.clone());
        let node_did = "did:key:zOrder";
        let at = |secs| chrono::DateTime::from_timestamp(secs, 0);
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
            Ok(gents::ConfigAccess::local(node)),
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
        let access = || Ok(gents::ConfigAccess::local(node.clone()));
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
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::local(node.clone()), node_did)
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
        let access = || Ok(gents::ConfigAccess::local(node.clone()));
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
        let stored = list_oauth_credentials_on(&gents::ConfigAccess::local(node.clone()), node_did)
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
        let access = || Ok(gents::ConfigAccess::local(node.clone()));
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
        let access = gents::ConfigAccess::local(node.clone());
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
pub(crate) fn desktop_grok_login_cancel<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    sign_in::cancel(&state, window.label(), InferenceAuthMethod::GrokOauth)
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeLoginRequest {
    pub node_did: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

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
pub(crate) async fn desktop_claude_login<R>(
    app: AppHandle<R>,
    window: tauri::WebviewWindow<R>,
    request: ClaudeLoginRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ClaudeLoginResult, BridgeError>
where
    R: Runtime,
{
    let provider =
        gents::claude_oauth::normalize_provider(request.provider.as_deref().unwrap_or_default());
    let signed = sign_in::run(
        &app,
        &state,
        window.label(),
        request.node_did.trim(),
        InferenceAuthMethod::ClaudeOauth,
        &provider,
        request.label.as_deref(),
    )
    .await?;
    Ok(ClaudeLoginResult::redacted(&signed))
}

#[tauri::command]
pub(crate) fn desktop_claude_login_cancel<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    sign_in::cancel(&state, window.label(), InferenceAuthMethod::ClaudeOauth)
}
